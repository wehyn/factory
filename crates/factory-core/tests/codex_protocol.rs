use factory_core::{CodexRunner, Event, EventKind, Ledger, SessionId, SessionProcessState};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};
use tempfile::TempDir;

struct FakeServer {
    _temp: TempDir,
    repo: PathBuf,
    turn_capture: PathBuf,
    ledger: Arc<Ledger>,
    session: SessionId,
}

impl FakeServer {
    fn new() -> anyhow::Result<Self> {
        let temp = tempfile::tempdir()?;
        let repo = temp.path().join("repo");
        std::fs::create_dir(&repo)?;
        std::fs::write(repo.join("README.md"), "Disposable protocol fixture.\n")?;
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()?;
        anyhow::ensure!(status.success(), "git init failed for fake server repo");

        let ledger = Arc::new(Ledger::open(&temp.path().join("ledger.sqlite"))?);
        let session = SessionId(uuid::Uuid::new_v4());
        ledger.append(&Event::new(session, EventKind::SessionCreated))?;

        Ok(Self {
            turn_capture: temp.path().join("turn.json"),
            _temp: temp,
            repo,
            ledger,
            session,
        })
    }

    fn command(&self, mode: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(FAKE_SERVER_SCRIPT)
            .arg("factory-fake-server")
            .arg(mode)
            .env("FACTORY_TURN_CAPTURE", &self.turn_capture)
            .env(
                "FACTORY_APP_SERVER_FIXTURE",
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tests/fixtures/app-server/fragmented.jsonl"),
            );
        command
    }

    fn repo(&self) -> &Path {
        &self.repo
    }

    fn ledger(&self) -> Arc<Ledger> {
        Arc::clone(&self.ledger)
    }

    fn session(&self) -> SessionId {
        self.session
    }

    fn turn_capture(&self) -> &Path {
        &self.turn_capture
    }
}

const FAKE_SERVER_SCRIPT: &str = r#"
set -eu
mode="$1"
fixture="$FACTORY_APP_SERVER_FIXTURE"
read_request() { IFS= read -r line || exit 21; }
request_id() { printf '%s\n' "$1" | sed -E 's/.*"id":([0-9]+).*/\1/'; }

read_request
id=$(request_id "$line")
printf '{"jsonrpc":"2.0","id":%s,' "$id"
sleep 0.01
printf '"result":{}}\n'

read_request # initialized notification
read_request
id=$(request_id "$line")
printf '{"jsonrpc":"2.0","id":%s,"result":{"thread":{"id":"01a0d9ed-60c2-74e1-93dc-bb9c1304615c"}}}\n' "$id"

read_request
id=$(request_id "$line")
if [ "$mode" = "workspace-policy" ]; then printf '%s\n' "$line" > "$FACTORY_TURN_CAPTURE"; fi
printf '{"jsonrpc":"2.0","id":%s,"result":{"turn":{"id":"01a0d9ed-6184-7d73-853f-05ad64f41b5c"}}}\n' "$id"

case "$mode" in
  success)
    cat "$fixture"
    ;;
  workspace-policy)
    cat "$fixture"
    ;;
  multi-turn)
    cat "$fixture"
    read_request
    id=$(request_id "$line")
    printf '{"jsonrpc":"2.0","id":%s,"result":{"turn":{"id":"01a0d9ed-6184-7d73-853f-05ad64f41b5f"}}}\n' "$id"
    sed -e 's/01a0d9ed-6184-7d73-853f-05ad64f41b5c/01a0d9ed-6184-7d73-853f-05ad64f41b5f/g' -e 's/"text":"hello"/"text":"second turn"/' "$fixture"
    ;;
  failure)
    sed '$d' "$fixture"
    ;;
  interrupt)
    read_request
    id=$(request_id "$line")
    printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
    sed '$d' "$fixture"
    printf '%s\n' '{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"01a0d9ed-60c2-74e1-93dc-bb9c1304615c","turn":{"id":"01a0d9ed-6184-7d73-853f-05ad64f41b5c","items":[],"status":"interrupted"}}}'
    ;;
  hold)
    sleep 30
    ;;
  oversized)
    printf '{"jsonrpc":"2.0","method":"unknown","params":{"data":"'
    dd if=/dev/zero bs=1048577 count=1 2>/dev/null | tr '\000' x
    printf '"}}\n'
    ;;
  *) exit 22 ;;
esac
"#;

#[test]
fn preserves_output_and_records_one_failure_when_server_exits_mid_turn() -> anyhow::Result<()> {
    let harness = FakeServer::new()?;
    let ledger = harness.ledger();
    let mut runner = CodexRunner::start_with_command(
        harness.command("failure"),
        harness.repo(),
        Arc::clone(&ledger),
        harness.session(),
    )?;
    runner.start_turn("Say hello without changing files")?;
    runner.wait_for_exit()?;
    runner.wait_for_exit()?;

    let snapshot = ledger.snapshot()?;
    let session = &snapshot.sessions[0];
    assert_eq!(session.output, vec!["hello"]);
    assert_eq!(session.failure_count, 1);
    assert_eq!(session.process_state, SessionProcessState::Failed);
    Ok(())
}

#[test]
fn persists_observed_turn_completion() -> anyhow::Result<()> {
    let harness = FakeServer::new()?;
    let ledger = harness.ledger();
    let mut runner = CodexRunner::start_with_command(
        harness.command("success"),
        harness.repo(),
        Arc::clone(&ledger),
        harness.session(),
    )?;
    runner.start_turn("Say hello without changing files")?;
    runner.wait_for_exit()?;

    let snapshot = ledger.snapshot()?;
    assert_eq!(snapshot.sessions[0].output, vec!["hello"]);
    assert_eq!(
        snapshot.sessions[0].process_state,
        SessionProcessState::Completed
    );
    Ok(())
}

#[test]
fn one_runner_accepts_multiple_sequential_turns_and_ignores_prior_turn_events() -> anyhow::Result<()>
{
    let harness = FakeServer::new()?;
    let ledger = harness.ledger();
    let mut runner = CodexRunner::start_with_command(
        harness.command("multi-turn"),
        harness.repo(),
        Arc::clone(&ledger),
        harness.session(),
    )?;
    runner.start_turn("First prompt")?;
    wait_until_completed(&ledger)?;
    runner.start_turn("Second prompt")?;
    runner.wait_for_exit()?;

    let snapshot = ledger.snapshot()?;
    assert_eq!(snapshot.sessions[0].output, vec!["hello", "second turn"]);
    assert_eq!(
        snapshot.sessions[0].process_state,
        SessionProcessState::Completed
    );
    let connection = rusqlite::Connection::open(ledger.database_path())?;
    let completed_turns: i64 = connection.query_row(
        "SELECT COUNT(*) FROM events WHERE session_id = ?1 AND kind = 'turn_completed'",
        [harness.session().to_string()],
        |row| row.get(0),
    )?;
    assert_eq!(completed_turns, 2);
    Ok(())
}

fn wait_until_completed(ledger: &Ledger) -> anyhow::Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let snapshot = ledger.snapshot()?;
        match snapshot
            .sessions
            .first()
            .map(|session| session.process_state)
        {
            Some(SessionProcessState::Completed) => return Ok(()),
            Some(SessionProcessState::Failed | SessionProcessState::Interrupted) => {
                anyhow::bail!("fake Codex turn did not complete successfully: {snapshot:?}")
            }
            _ if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            _ => anyhow::bail!("fake Codex turn did not complete before the timeout"),
        }
    }
}

#[test]
fn worker_turn_limits_writes_to_its_worktree_and_disables_network() -> anyhow::Result<()> {
    let harness = FakeServer::new()?;
    let ledger = harness.ledger();
    let mut runner = CodexRunner::start_with_command(
        harness.command("workspace-policy"),
        harness.repo(),
        Arc::clone(&ledger),
        harness.session(),
    )?;
    runner.start_turn_in_worktree("Make the scoped change", harness.repo())?;
    runner.wait_for_exit()?;

    let request: serde_json::Value =
        serde_json::from_slice(&std::fs::read(harness.turn_capture())?)?;
    let policy = request
        .pointer("/params/sandboxPolicy")
        .expect("worker sandbox policy");
    assert_eq!(policy["type"], "workspaceWrite");
    assert_eq!(
        policy["writableRoots"][0],
        harness.repo().canonicalize()?.to_string_lossy().as_ref()
    );
    assert_eq!(policy["networkAccess"], false);
    Ok(())
}

#[test]
fn interrupt_records_the_observed_interrupted_terminal_state() -> anyhow::Result<()> {
    let harness = FakeServer::new()?;
    let ledger = harness.ledger();
    let mut runner = CodexRunner::start_with_command(
        harness.command("interrupt"),
        harness.repo(),
        Arc::clone(&ledger),
        harness.session(),
    )?;
    runner.start_turn("Say hello without changing files")?;
    runner.interrupt()?;
    runner.wait_for_exit()?;

    assert_eq!(
        ledger.snapshot()?.sessions[0].process_state,
        SessionProcessState::Interrupted
    );
    Ok(())
}

#[test]
fn shutdown_terminates_the_server_process_group() -> anyhow::Result<()> {
    let harness = FakeServer::new()?;
    let ledger = harness.ledger();
    let mut runner = CodexRunner::start_with_command(
        harness.command("hold"),
        harness.repo(),
        Arc::clone(&ledger),
        harness.session(),
    )?;
    runner.start_turn("Say hello without changing files")?;
    let started_at = std::time::Instant::now();
    runner.shutdown()?;

    assert!(started_at.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(
        ledger.snapshot()?.sessions[0].process_state,
        SessionProcessState::Interrupted
    );
    Ok(())
}

#[test]
fn failed_to_spawn_server_marks_session_failed() -> anyhow::Result<()> {
    let harness = FakeServer::new()?;
    let ledger = harness.ledger();
    let missing_executable = harness.repo().join("missing-codex-server");
    let result = CodexRunner::start_with_command(
        Command::new(missing_executable),
        harness.repo(),
        Arc::clone(&ledger),
        harness.session(),
    );

    assert!(result.is_err());
    let session = &ledger.snapshot()?.sessions[0];
    assert_eq!(session.process_state, SessionProcessState::Failed);
    assert_eq!(session.failure_count, 1);
    Ok(())
}

#[test]
fn rejects_oversized_jsonl_and_records_a_single_failure() -> anyhow::Result<()> {
    let harness = FakeServer::new()?;
    let ledger = harness.ledger();
    let mut runner = CodexRunner::start_with_command(
        harness.command("oversized"),
        harness.repo(),
        Arc::clone(&ledger),
        harness.session(),
    )?;
    runner.start_turn("Say hello without changing files")?;
    assert!(runner.wait_for_exit().is_err());
    assert_eq!(ledger.snapshot()?.sessions[0].failure_count, 1);
    Ok(())
}

#[test]
#[ignore = "requires a signed-in local Codex CLI and network access"]
fn live_codex_app_server_persists_a_harmless_disposable_turn() -> anyhow::Result<()> {
    use std::time::{Duration, Instant};

    let temp = tempfile::tempdir()?;
    let repo = temp.path().join("disposable-repo");
    std::fs::create_dir(&repo)?;
    std::fs::write(
        repo.join("README.md"),
        "A disposable, read-only Codex probe.\n",
    )?;
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .status()?;
    anyhow::ensure!(status.success(), "git init failed for live probe repo");

    let ledger = Arc::new(Ledger::open(&temp.path().join("ledger.sqlite"))?);
    let session = SessionId(uuid::Uuid::new_v4());
    ledger.append(&Event::new(session, EventKind::SessionCreated))?;
    let mut runner = CodexRunner::start(&repo, Arc::clone(&ledger), session)?;
    runner.start_turn("Reply with exactly the word hello. Do not use tools or modify files.")?;

    let deadline = Instant::now() + Duration::from_secs(120);
    let snapshot = loop {
        let snapshot = ledger.snapshot()?;
        let session = &snapshot.sessions[0];
        if session.process_state == SessionProcessState::Completed {
            break snapshot;
        }
        if session.process_state == SessionProcessState::Failed {
            anyhow::bail!("live Codex probe failed: {snapshot:?}");
        }
        if Instant::now() >= deadline {
            anyhow::bail!("live Codex probe did not complete before the 120 second timeout");
        }
        std::thread::sleep(Duration::from_millis(200));
    };

    assert!(snapshot.sessions[0]
        .output
        .iter()
        .any(|output| output.trim() == "hello"));
    runner.shutdown()?;
    Ok(())
}
