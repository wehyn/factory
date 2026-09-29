use anyhow::Context;
use factory_core::{
    CodexRunner, Event, EventKind, FactorySnapshot, Ledger, RedactedOutput, SessionId,
    WorktreeManager,
};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Emitter, Manager,
};

const DISPOSABLE_README: &str = "Agentic Factory disposable read-only session fixture.\n";
const DEFAULT_PROMPT_MAX_CHARS: usize = 16_000;

struct AppState {
    ledger: Arc<Ledger>,
    disposable_repo: PathBuf,
    runner: Mutex<Option<CodexRunner>>,
}

impl AppState {
    fn shutdown(&self) {
        if let Ok(mut runner) = self.runner.lock() {
            if let Some(runner) = runner.as_mut() {
                let _ = runner.shutdown();
            }
        }
    }
}

#[tauri::command]
fn show_main_window(app: tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "main window missing".to_string())?;
    window.show().map_err(|error| error.to_string())?;
    window.set_focus().map_err(|error| error.to_string())
}

#[tauri::command]
fn get_snapshot(state: tauri::State<'_, AppState>) -> Result<FactorySnapshot, String> {
    state.ledger.snapshot().map_err(safe_error)
}

#[tauri::command]
fn get_disposable_repo_path(state: tauri::State<'_, AppState>) -> String {
    state.disposable_repo.to_string_lossy().into_owned()
}

#[tauri::command]
fn start_disposable_session(
    state: tauri::State<'_, AppState>,
    repo_path: String,
    prompt: String,
) -> Result<SessionId, String> {
    let supplied_path = PathBuf::from(repo_path)
        .canonicalize()
        .map_err(|error| safe_error(anyhow::anyhow!(error)))?;
    if supplied_path != state.disposable_repo {
        return Err("Only the configured disposable repository can start a session".to_owned());
    }

    let prompt = prompt.trim();
    if prompt.is_empty() {
        return Err("Enter a prompt for the disposable session".to_owned());
    }
    if prompt.chars().count() > DEFAULT_PROMPT_MAX_CHARS {
        return Err("Prompt exceeds the 16,000 character limit".to_owned());
    }

    let mut runner_slot = state
        .runner
        .lock()
        .map_err(|_| "Codex session manager lock was poisoned".to_owned())?;
    if runner_slot.is_some() {
        return Err("The disposable session has already been started".to_owned());
    }
    let existing = state.ledger.snapshot().map_err(safe_error)?;
    if !existing.sessions.is_empty() {
        return Err("This installation already has its one disposable session".to_owned());
    }

    let session_id = SessionId(uuid::Uuid::new_v4());
    state
        .ledger
        .append(&Event::new(session_id, EventKind::SessionCreated))
        .map_err(safe_error)?;

    let mut runner = CodexRunner::start(
        &state.disposable_repo,
        Arc::clone(&state.ledger),
        session_id,
    )
    .map_err(safe_error)?;
    if let Err(error) = runner.start_turn(prompt) {
        let _ = runner.shutdown();
        return Err(safe_error(error));
    }
    *runner_slot = Some(runner);
    Ok(session_id)
}

fn safe_error(error: impl std::fmt::Display) -> String {
    RedactedOutput::new(error.to_string()).as_str().to_owned()
}

fn prepare_disposable_repo(app_data_dir: &Path) -> anyhow::Result<PathBuf> {
    let repo = app_data_dir.join("disposable-session");
    if !repo.exists() {
        std::fs::create_dir(&repo)?;
        std::fs::write(repo.join("README.md"), DISPOSABLE_README)?;
        std::fs::write(repo.join(".agentic-factory-disposable"), "v1\n")?;

        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("initializing disposable Git repository")?;
        anyhow::ensure!(
            status.success(),
            "git init failed for the disposable repository"
        );
    }

    let canonical_repo = repo.canonicalize()?;
    anyhow::ensure!(
        canonical_repo.join(".git").exists()
            && canonical_repo.join("README.md").is_file()
            && canonical_repo.join(".agentic-factory-disposable").is_file(),
        "the configured disposable repository is missing its application marker or Git metadata"
    );
    let marker = std::fs::read_to_string(canonical_repo.join(".agentic-factory-disposable"))?;
    anyhow::ensure!(
        marker == "v1\n",
        "the disposable repository marker is invalid"
    );
    Ok(canonical_repo)
}

fn initialize_app_state(app: &mut tauri::App) -> anyhow::Result<AppState> {
    let app_data_dir = app.path().app_local_data_dir()?;
    std::fs::create_dir_all(&app_data_dir)?;
    let app_data_dir = app_data_dir.canonicalize()?;
    let ledger = Arc::new(Ledger::open(&app_data_dir.join("events.sqlite"))?);

    // No child process survives a service restart. Record that recovery before the webview
    // can request its first snapshot, so persisted start events are never exposed as live.
    ledger.recover_unfinished_sessions()?;
    let worktrees = WorktreeManager::new((*ledger).clone(), app_data_dir.join("worktrees"))?;
    worktrees.reconcile()?;
    let disposable_repo = prepare_disposable_repo(&app_data_dir)?;

    let event_receiver = ledger.subscribe()?;
    let app_handle = app.handle().clone();
    thread::Builder::new()
        .name("factory-event-emitter".to_owned())
        .spawn(move || {
            while let Ok(event) = event_receiver.recv() {
                let _ = app_handle.emit("factory-event", event);
            }
        })?;

    Ok(AppState {
        ledger,
        disposable_repo,
        runner: Mutex::new(None),
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .setup(|app| {
            let state = initialize_app_state(app)?;
            app.manage(state);

            let open = MenuItem::with_id(app, "open", "Open Agentic Factory", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Agentic Factory", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;
            let icon = app
                .default_window_icon()
                .cloned()
                .expect("default app icon is configured");

            TrayIconBuilder::new()
                .icon(icon)
                .menu(&menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => {
                        if let Some(state) = app.try_state::<AppState>() {
                            state.shutdown();
                        }
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            show_main_window,
            get_snapshot,
            get_disposable_repo_path,
            start_disposable_session,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Agentic Factory");

    app.run(|app_handle, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            if let Some(state) = app_handle.try_state::<AppState>() {
                state.shutdown();
            }
        }
    });
}
