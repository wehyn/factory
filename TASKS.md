# Resident Codex Session Execution Ledger

Base commit: `a1d2fbc1918dbaa0e9878ac4ebaf2c3f955dfc62` (preserved design workspace)
Owner: root agent

## Task 1: Scaffold and resident window lifecycle

- Owner: root
- Scope: `package.json`, frontend config, `src/`, `src-tauri/`, `README.md`, `tests/manual/window-lifecycle.md`
- Dependencies: none
- Status: COMPLETE
- Acceptance: pinned scaffold versions documented; `npm run build`, `cargo test --workspace`, and `npm run tauri dev` work; closing the window hides it, tray Open restores it, and tray Quit exits the same process.
- Verification evidence: `npm run build`, `cargo test --workspace`, `npm run tauri dev`, and the packaged release build passed. Closing the main window hid it while PID 49185 remained; tray Open restored the same PID and visible window; tray Quit exited that PID and its Codex App Server child. A subsequent release launch restored the completed session.
- Commit: 3f469f182ed605c052f1a0e93f0fadc2030d1a5c

## Task 2: Typed durable event ledger

- Owner: root
- Scope: root Cargo workspace, `crates/factory-core/src/model.rs`, `crates/factory-core/src/ledger.rs`, ledger recovery tests, `src-tauri/Cargo.toml`
- Dependencies: Task 1
- Status: COMPLETE
- Acceptance: ordered idempotent append, snapshot recovery after reopening SQLite, and credential redaction before persistence.
- Verification evidence: final `cargo test --workspace` passed all 8 ledger tests, including ordered reopen, idempotency/conflict rejection, redaction before DB/WAL storage, cross-connection snapshot sequencing, restart reconciliation, ordered event delivery under a concurrent 400-event burst, and bounded recovered output. The credential regression was observed failing before the redaction fix.
- Commit: b7fa93e0630701a5916b195746d4ff0f2ebd8d1b

## Task 3: Codex App Server supervision

- Owner: root
- Scope: `crates/factory-core/src/codex.rs`, protocol tests/fixtures, `tests/manual/app-server-probe.md`
- Dependencies: Task 2
- Status: COMPLETE
- Acceptance: current installed protocol is confirmed; fragmented JSONL, output preservation, turn completion, interrupt, and exactly-once failure behavior are handled.
- Verification evidence: fake App Server tests passed for fragmented JSONL, output preservation, completion, interruption, exactly-once crash failure, oversized-line rejection, process-group shutdown, spawn failure, and late-notification terminal-state guards. The live adapter test passed twice on `codex-cli 0.157.0` in a disposable read-only Git repo; the latest run completed in 10.74 seconds, persisted real `agentMessage` text `hello` and `turn/completed`, and left no App Server process running.
- Commit: 1a1b3a4bea4cd9541e2f1e83f83873e79ce101cd

## Task 4: Reconnect the read-only UI

- Owner: root
- Scope: `src/bridge.ts`, `src/App.tsx`, `src/styles.css`, `src-tauri/src/lib.rs`, UI tests, manual acceptance records
- Dependencies: Tasks 1, 2, and 3
- Status: COMPLETE
- Acceptance: snapshot loads on reconnect, event gaps reload from the ledger, only disposable-repository turns can be started, and session output remains read-only and redacted.
- Verification evidence: frontend tests passed snapshot restoration, output delivery, read-only UI, gap reload, and the 40-output live-state bound. `npm run build`, `cargo test --workspace`, `cargo fmt --all -- --check`, `git diff --check`, and `npm run tauri build` passed. The packaged app completed the harmless real Codex turn and displayed `hello`; closing hid the window without exiting PID 49185; tray Open restored the same PID and output; tray Quit exited the app and App Server. After relaunch, the final build restored the same five-event completed snapshot in PID 55617 without starting another Codex process.
- Commit: 40b659be0cae3a0d78b6dc92832de133aba3aec3
