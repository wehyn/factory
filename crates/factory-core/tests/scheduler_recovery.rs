use anyhow::{Context, Result};
use factory_core::{
    AgentId, AssignSliceRequest, CodexWorkerLauncher, Event, EventKind, Ledger, Mailbox,
    RepositoryRegistry, RunId, Scheduler, SessionId, SessionProcessState, SliceAssignment, SliceId,
    SliceStatus, WorkerExit, WorkerLauncher, WorkerProcess, Worktree, WorktreeManager,
};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;
use uuid::Uuid;

#[derive(Default)]
struct FakeLauncher {
    ledger: Mutex<Option<Arc<Ledger>>>,
    starts: Mutex<Vec<(SliceId, AgentId, SessionId)>>,
}

impl FakeLauncher {
    fn with_ledger(ledger: Arc<Ledger>) -> Self {
        Self {
            ledger: Mutex::new(Some(ledger)),
            starts: Mutex::new(Vec::new()),
        }
    }

    fn count(&self) -> usize {
        self.starts.lock().unwrap().len()
    }
}

struct FakeProcess;

impl WorkerProcess for FakeProcess {
    fn shutdown(&mut self) -> Result<()> {
        Ok(())
    }

    fn poll_exit(&mut self) -> Result<Option<WorkerExit>> {
        Ok(None)
    }
}

impl WorkerLauncher for FakeLauncher {
    fn launch(
        &self,
        assignment: &SliceAssignment,
        _worktree: &Worktree,
        session_id: SessionId,
    ) -> Result<Box<dyn WorkerProcess>> {
        self.starts
            .lock()
            .unwrap()
            .push((assignment.id, assignment.agent_id, session_id));
        let ledger = self.ledger.lock().unwrap().clone().expect("fake ledger");
        ledger.append(&Event::new(
            session_id,
            EventKind::SessionStarted {
                thread_id: format!("fake-{session_id}"),
            },
        ))?;
        ledger.append(&Event::new(
            session_id,
            EventKind::TurnStarted {
                turn_id: format!("turn-{session_id}"),
            },
        ))?;
        ledger.append(&Event::new(session_id, EventKind::TurnCompleted))?;
        Ok(Box::new(FakeProcess))
    }
}

struct Fixture {
    _temp: TempDir,
    base_sha: String,
    ledger_path: std::path::PathBuf,
    ledger: Arc<Ledger>,
    worktrees: WorktreeManager,
    mailbox: Mailbox,
    run_id: RunId,
}

impl Fixture {
    fn new() -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let repository = temp.path().join("repository");
        fs::create_dir_all(repository.join("src"))?;
        fs::create_dir_all(repository.join("tests"))?;
        git(&repository, &["init", "--initial-branch=main"])?;
        git(&repository, &["config", "user.name", "Factory Test"])?;
        git(
            &repository,
            &["config", "user.email", "factory-test@example.invalid"],
        )?;
        fs::write(repository.join("README.md"), "scheduler fixture\n")?;
        fs::write(repository.join("src/api.rs"), "pub fn api() {}\n")?;
        fs::write(repository.join("src/ui.rs"), "pub fn ui() {}\n")?;
        fs::write(repository.join("tests/verify.md"), "base\n")?;
        git(&repository, &["add", "."])?;
        git(&repository, &["commit", "-m", "initial"])?;
        let base_sha = git(&repository, &["rev-parse", "HEAD"])?;

        let ledger_path = temp.path().join("factory.sqlite");
        let ledger = Arc::new(Ledger::open(&ledger_path)?);
        let registry = RepositoryRegistry::new((*ledger).clone());
        let worktrees = WorktreeManager::new((*ledger).clone(), temp.path().join("worktrees"))?;
        let mailbox = Mailbox::new((*ledger).clone(), worktrees.clone());
        let registered = registry.register(&repository)?;
        let run_id = RunId(Uuid::new_v4());
        worktrees.create_run_worktree(registered.id, run_id, &base_sha)?;
        Ok(Self {
            _temp: temp,
            base_sha,
            ledger_path,
            ledger,
            worktrees,
            mailbox,
            run_id,
        })
    }

    fn scheduler(&self, launcher: Arc<dyn WorkerLauncher>) -> Scheduler {
        Scheduler::new(Arc::clone(&self.ledger), self.worktrees.clone(), launcher)
    }

    fn assign(&self, key: &str, path: &str, dependencies: Vec<SliceId>) -> Result<SliceAssignment> {
        self.assign_with_details(
            key,
            path,
            dependencies,
            format!("Implement {key}"),
            format!("Verify {key}"),
        )
    }

    fn assign_with_details(
        &self,
        key: &str,
        path: &str,
        dependencies: Vec<SliceId>,
        objective: String,
        acceptance_evidence: String,
    ) -> Result<SliceAssignment> {
        Ok(self.mailbox.assign_slice(
            factory_core::McpPrincipal::Manager,
            AssignSliceRequest {
                run_id: self.run_id,
                assignment_key: key.to_owned(),
                objective,
                allowed_paths: vec![path.to_owned()],
                dependency_ids: dependencies,
                contract_keys: Vec::new(),
                acceptance_evidence,
            },
        )?)
    }

    fn finish_worker(
        &self,
        assignment: &SliceAssignment,
        path: &str,
        contents: &str,
    ) -> Result<()> {
        let worktree = self.write_worker_file(assignment, path, contents)?;
        git(&worktree.path, &["add", path])?;
        git(
            &worktree.path,
            &[
                "commit",
                "-m",
                &format!("complete {}", assignment.assignment_key),
            ],
        )?;
        Ok(())
    }

    fn write_worker_file(
        &self,
        assignment: &SliceAssignment,
        path: &str,
        contents: &str,
    ) -> Result<Worktree> {
        let worktree = self
            .worktrees
            .get_worktree(assignment.worktree_id.expect("assigned worktree"))?;
        let file = worktree.path.join(path);
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(file, contents)?;
        Ok(worktree)
    }
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[test]
fn schedules_parallel_slices_and_waits_for_every_dependency_before_verification() -> Result<()> {
    let fixture = Fixture::new()?;
    let api = fixture.assign("api", "src/api.rs", vec![])?;
    let ui = fixture.assign("ui", "src/ui.rs", vec![])?;
    let verification = fixture.assign("verification", "tests/verify.md", vec![api.id, ui.id])?;
    let launcher = Arc::new(FakeLauncher::with_ledger(Arc::clone(&fixture.ledger)));
    let scheduler = fixture.scheduler(launcher.clone());

    let ready = scheduler.ready(fixture.run_id)?;
    assert_eq!(ready.len(), 2);
    assert!(ready.contains(&api.id));
    assert!(ready.contains(&ui.id));
    assert_eq!(scheduler.spawn_slice(api.id)?, api.agent_id);
    assert_eq!(scheduler.spawn_slice(ui.id)?, ui.agent_id);
    assert!(scheduler.spawn_slice(api.id).is_err());
    assert_eq!(launcher.count(), 2);

    fixture.finish_worker(&api, "src/api.rs", "pub fn api() { println!(\"api\"); }\n")?;
    scheduler.complete_slice(api.agent_id, "API implementation committed.")?;
    assert!(!scheduler.ready(fixture.run_id)?.contains(&verification.id));
    assert_eq!(scheduler.integrate_completed(fixture.run_id)?.len(), 1);

    fixture.finish_worker(&ui, "src/ui.rs", "pub fn ui() { println!(\"ui\"); }\n")?;
    scheduler.complete_slice(ui.agent_id, "UI implementation committed.")?;
    assert!(!scheduler.ready(fixture.run_id)?.contains(&verification.id));
    assert_eq!(scheduler.integrate_completed(fixture.run_id)?.len(), 1);
    assert_eq!(scheduler.ready(fixture.run_id)?, vec![verification.id]);

    scheduler.spawn_slice(verification.id)?;
    let verification_worktree = fixture
        .worktrees
        .get_worktree(verification.worktree_id.expect("verification worktree"))?;
    assert_ne!(verification_worktree.base_sha, fixture.base_sha);
    assert!(
        git(&verification_worktree.path, &["show", "HEAD:src/api.rs"])?
            .contains("println!(\"api\")")
    );
    assert!(
        git(&verification_worktree.path, &["show", "HEAD:src/ui.rs"])?.contains("println!(\"ui\")")
    );
    fixture.finish_worker(&verification, "tests/verify.md", "verified\n")?;
    scheduler.complete_slice(verification.agent_id, "Combined verification passed.")?;
    assert!(scheduler.integration_ready(fixture.run_id)?);
    Ok(())
}

#[test]
fn restart_blocks_an_uncertain_worker_instead_of_allocating_a_duplicate() -> Result<()> {
    let fixture = Fixture::new()?;
    let slice = fixture.assign("uncertain", "src/api.rs", vec![])?;
    let first_launcher = Arc::new(FakeLauncher::with_ledger(Arc::clone(&fixture.ledger)));
    let scheduler = fixture.scheduler(first_launcher.clone());
    scheduler.spawn_slice(slice.id)?;
    drop(scheduler);
    assert_eq!(first_launcher.count(), 1);

    let reopened_ledger = Arc::new(Ledger::open(&fixture.ledger_path)?);
    let reopened_worktrees = WorktreeManager::new(
        (*reopened_ledger).clone(),
        fixture._temp.path().join("worktrees"),
    )?;
    let recovered_launcher = Arc::new(FakeLauncher::with_ledger(Arc::clone(&reopened_ledger)));
    let recovered = Scheduler::new(
        Arc::clone(&reopened_ledger),
        reopened_worktrees,
        recovered_launcher.clone(),
    );
    recovered.reconcile_after_restart()?;

    assert!(recovered.ready(fixture.run_id)?.is_empty());
    assert_eq!(
        recovered.get_assignment(slice.id)?.status,
        SliceStatus::Blocked
    );
    assert!(recovered.spawn_slice(slice.id).is_err());
    assert_eq!(recovered_launcher.count(), 0);
    let blockers = recovered.list_blockers(fixture.run_id)?;
    assert_eq!(blockers.len(), 1);
    assert!(blockers[0].detail.contains("uncertain"));
    Ok(())
}

#[test]
fn retries_are_bounded_and_dirty_worktrees_become_manager_blockers() -> Result<()> {
    let fixture = Fixture::new()?;
    let slice = fixture.assign("retry", "src/api.rs", vec![])?;
    let launcher = Arc::new(FakeLauncher::with_ledger(Arc::clone(&fixture.ledger)));
    let scheduler = fixture.scheduler(launcher.clone());

    scheduler.spawn_slice(slice.id)?;
    scheduler.process_exited(slice.agent_id, WorkerExit::Failed)?;
    assert_eq!(
        scheduler.get_assignment(slice.id)?.status,
        SliceStatus::Retryable
    );
    scheduler.retry_slice(slice.id)?;
    scheduler.spawn_slice(slice.id)?;
    let worktree = fixture
        .worktrees
        .get_worktree(slice.worktree_id.expect("worktree"))?;
    fs::write(
        worktree.path.join("partial.txt"),
        "uncommitted worker output\n",
    )?;
    scheduler.process_exited(slice.agent_id, WorkerExit::Interrupted)?;

    assert_eq!(
        scheduler.get_assignment(slice.id)?.status,
        SliceStatus::Blocked
    );
    assert!(scheduler.retry_slice(slice.id).is_err());
    assert_eq!(launcher.count(), 2);
    assert_eq!(scheduler.list_blockers(fixture.run_id)?.len(), 1);
    Ok(())
}

#[test]
fn integration_cherry_picks_only_completed_scoped_commits_and_records_both_shas() -> Result<()> {
    let fixture = Fixture::new()?;
    let slice = fixture.assign("integrate-api", "src/api.rs", vec![])?;
    let launcher = Arc::new(FakeLauncher::with_ledger(Arc::clone(&fixture.ledger)));
    let scheduler = fixture.scheduler(launcher);
    scheduler.spawn_slice(slice.id)?;
    fixture.finish_worker(
        &slice,
        "src/api.rs",
        "pub fn api() { println!(\"integrated\"); }\n",
    )?;
    let source_worktree = fixture
        .worktrees
        .get_worktree(slice.worktree_id.expect("worktree"))?;
    std::fs::OpenOptions::new()
        .append(true)
        .open(source_worktree.path.join("src/api.rs"))?
        .write_all(b"// second worker commit\n")?;
    git(&source_worktree.path, &["add", "src/api.rs"])?;
    git(&source_worktree.path, &["commit", "-m", "worker follow-up"])?;
    let source_sha = git(&source_worktree.path, &["rev-parse", "HEAD"])?;
    scheduler.complete_slice(slice.agent_id, "API slice committed.")?;

    let records = scheduler.integrate_completed(fixture.run_id)?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].slice_id, slice.id);
    assert_eq!(records[0].source_commit, source_sha);
    let destination_commit = records[0]
        .destination_commit
        .as_deref()
        .expect("integration destination commit");
    assert_ne!(destination_commit, fixture.base_sha);
    let integration = fixture.worktrees.integration_worktree(fixture.run_id)?;
    assert_eq!(
        git(&integration.path, &["rev-parse", "HEAD"])?,
        destination_commit
    );
    let integrated = git(&integration.path, &["show", "HEAD:src/api.rs"])?;
    assert!(integrated.contains("integrated"));
    assert!(integrated.contains("second worker commit"));
    assert!(scheduler.integrate_completed(fixture.run_id)?.is_empty());
    Ok(())
}

#[test]
fn manager_captures_scoped_worker_edits_as_a_source_commit() -> Result<()> {
    let fixture = Fixture::new()?;
    let slice = fixture.assign("capture-api", "src/api.rs", vec![])?;
    let launcher = Arc::new(FakeLauncher::with_ledger(Arc::clone(&fixture.ledger)));
    let scheduler = fixture.scheduler(launcher);
    scheduler.spawn_slice(slice.id)?;
    let worker = fixture.write_worker_file(
        &slice,
        "src/api.rs",
        "pub fn api() -> &'static str { \"captured\" }\n",
    )?;

    let completed = scheduler.complete_slice(slice.agent_id, "API implementation is ready.")?;
    let source_commit = completed.source_commit.expect("manager source commit");
    assert_ne!(source_commit, worker.base_sha);
    assert_eq!(git(&worker.path, &["rev-parse", "HEAD"])?, source_commit);
    assert_eq!(
        fixture.worktrees.inspect_worktree(worker.id)?,
        factory_core::WorktreeStatus::Clean
    );
    assert_eq!(scheduler.integrate_completed(fixture.run_id)?.len(), 1);
    Ok(())
}

#[test]
fn completion_with_changes_outside_the_assigned_scope_becomes_a_manager_blocker() -> Result<()> {
    let fixture = Fixture::new()?;
    let slice = fixture.assign("scope-check", "src/api.rs", vec![])?;
    let launcher = Arc::new(FakeLauncher::with_ledger(Arc::clone(&fixture.ledger)));
    let scheduler = fixture.scheduler(launcher);
    scheduler.spawn_slice(slice.id)?;
    fixture.finish_worker(
        &slice,
        "src/api.rs",
        "pub fn api() { println!(\"api\"); }\n",
    )?;

    let worker = fixture
        .worktrees
        .get_worktree(slice.worktree_id.expect("worker worktree"))?;
    fs::write(worker.path.join("src/ui.rs"), "unauthorized change\n")?;
    git(&worker.path, &["add", "src/ui.rs"])?;
    git(&worker.path, &["commit", "-m", "out of scope edit"])?;

    assert!(scheduler
        .complete_slice(slice.agent_id, "I changed another file.")
        .is_err());
    assert_eq!(
        scheduler.get_assignment(slice.id)?.status,
        SliceStatus::Blocked
    );
    let blockers = scheduler.list_blockers(fixture.run_id)?;
    assert_eq!(blockers.len(), 1);
    assert!(blockers[0].detail.contains("outside its assigned scope"));
    assert!(!scheduler.integration_ready(fixture.run_id)?);
    Ok(())
}

#[test]
#[ignore = "requires a signed-in local Codex CLI and network access"]
fn two_real_workers_commit_in_parallel_and_integrate_into_the_run_worktree() -> Result<()> {
    let fixture = Fixture::new()?;
    let api = fixture.assign_with_details(
        "live-api",
        "src/api.rs",
        vec![],
        "Replace the no-op in src/api.rs with exactly `pub fn api() -> &'static str { \"api-ready\" }`. Keep the change limited to src/api.rs.".to_owned(),
        "Verify src/api.rs contains the api function returning api-ready. Send completion evidence; the Factory manager records the scoped edits as a commit.".to_owned(),
    )?;
    let ui = fixture.assign_with_details(
        "live-ui",
        "src/ui.rs",
        vec![],
        "Replace the no-op in src/ui.rs with exactly `pub fn ui() -> &'static str { \"ui-ready\" }`. Keep the change limited to src/ui.rs.".to_owned(),
        "Verify src/ui.rs contains the ui function returning ui-ready. Send completion evidence; the Factory manager records the scoped edits as a commit.".to_owned(),
    )?;
    let launcher = Arc::new(CodexWorkerLauncher::new(
        Arc::clone(&fixture.ledger),
        &fixture.worktrees,
        PathBuf::from(env!("CARGO_BIN_EXE_factory-mcp-server")),
        Vec::new(),
    ));
    let scheduler = fixture.scheduler(launcher);

    scheduler.spawn_slice(api.id)?;
    scheduler.spawn_slice(ui.id)?;

    let api_session = attempt_session_id(&fixture.ledger_path, api.id)?;
    let ui_session = attempt_session_id(&fixture.ledger_path, ui.id)?;
    let deadline = Instant::now() + Duration::from_secs(240);
    loop {
        let snapshot = fixture.ledger.snapshot()?;
        let api_state = snapshot
            .sessions
            .iter()
            .find(|session| session.session_id == api_session)
            .map(|session| session.process_state);
        let ui_state = snapshot
            .sessions
            .iter()
            .find(|session| session.session_id == ui_session)
            .map(|session| session.process_state);
        if api_state == Some(SessionProcessState::Completed)
            && ui_state == Some(SessionProcessState::Completed)
        {
            println!(
                "API worker output:\n{}",
                snapshot
                    .sessions
                    .iter()
                    .find(|session| session.session_id == api_session)
                    .map(|session| session.output.join("\n"))
                    .unwrap_or_default()
            );
            println!(
                "UI worker output:\n{}",
                snapshot
                    .sessions
                    .iter()
                    .find(|session| session.session_id == ui_session)
                    .map(|session| session.output.join("\n"))
                    .unwrap_or_default()
            );
            break;
        }
        if matches!(
            api_state,
            Some(SessionProcessState::Failed | SessionProcessState::Interrupted)
        ) || matches!(
            ui_state,
            Some(SessionProcessState::Failed | SessionProcessState::Interrupted)
        ) {
            anyhow::bail!("a real Codex worker failed or was interrupted");
        }
        if Instant::now() >= deadline {
            anyhow::bail!("real Codex workers did not finish within four minutes");
        }
        thread::sleep(Duration::from_millis(500));
    }

    scheduler
        .complete_slice(api.agent_id, "The API worker committed src/api.rs.")
        .context("completing the API worker")?;
    scheduler
        .complete_slice(ui.agent_id, "The UI worker committed src/ui.rs.")
        .context("completing the UI worker")?;
    let messages = fixture.mailbox.list_messages(fixture.run_id)?;
    assert!(messages.iter().any(|message| {
        message.from == factory_core::MessageRecipient::Agent(api.agent_id)
            && message.to == factory_core::MessageRecipient::Manager
            && message.kind == factory_core::MessageKind::Completion
    }));
    assert!(messages.iter().any(|message| {
        message.from == factory_core::MessageRecipient::Agent(ui.agent_id)
            && message.to == factory_core::MessageRecipient::Manager
            && message.kind == factory_core::MessageKind::Completion
    }));

    assert_eq!(scheduler.integrate_completed(fixture.run_id)?.len(), 2);
    let integration = fixture.worktrees.integration_worktree(fixture.run_id)?;
    assert!(git(&integration.path, &["show", "HEAD:src/api.rs"])?.contains("api-ready"));
    assert!(git(&integration.path, &["show", "HEAD:src/ui.rs"])?.contains("ui-ready"));
    scheduler.shutdown()?;
    Ok(())
}

fn attempt_session_id(ledger_path: &Path, slice_id: SliceId) -> Result<SessionId> {
    let connection = rusqlite::Connection::open(ledger_path)?;
    let session_id = connection.query_row(
        "SELECT session_id FROM slice_attempts WHERE slice_id = ?1 ORDER BY attempt_number DESC LIMIT 1",
        [slice_id.to_string()],
        |row| row.get::<_, String>(0),
    )?;
    Ok(SessionId(Uuid::parse_str(&session_id)?))
}
