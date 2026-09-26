# Resident Codex Session Execution Ledger

Base commit: `a1d2fbc1918dbaa0e9878ac4ebaf2c3f955dfc62` (preserved design workspace)
Owner: root agent

## Task 1: Scaffold and resident window lifecycle

- Owner: root
- Scope: `package.json`, frontend config, `src/`, `src-tauri/`, `README.md`, `tests/manual/window-lifecycle.md`
- Dependencies: none
- Status: IN_PROGRESS
- Acceptance: pinned scaffold versions documented; `npm run build`, `cargo test --workspace`, and `npm run tauri dev` work; closing the window hides it, tray Open restores it, and tray Quit exits the same process.
- Verification evidence: npm run build passed; cargo test --manifest-path src-tauri/Cargo.toml passed; release .app built. Baseline close terminated the scaffold process. After the handler change, Cmd-W hid the app while PID 30593 remained. The menu-bar Open and Quit items are implemented and compile, but CUA cannot access a no-window menu extra to click them yet.
- Commit: 3f469f182ed605c052f1a0e93f0fadc2030d1a5c (scaffold committed; manual Open/Quit click remains unverified)

## Task 2: Typed durable event ledger

- Owner: root
- Scope: root Cargo workspace, `crates/factory-core/src/model.rs`, `crates/factory-core/src/ledger.rs`, ledger recovery tests, `src-tauri/Cargo.toml`
- Dependencies: Task 1
- Status: COMPLETE
- Acceptance: ordered idempotent append, snapshot recovery after reopening SQLite, and credential redaction before persistence.
- Verification evidence: cargo test --workspace passed 5 ledger tests, including ordered reopen, idempotency/conflict rejection, redaction of Authorization/API keys/prefixed environment keys before DB/WAL storage, cross-connection snapshot sequencing, and restart reconciliation of orphaned sessions. The credential regression was observed failing before the redaction fix.
- Commit: b7fa93e0630701a5916b195746d4ff0f2ebd8d1b

## Task 3: Codex App Server supervision

- Owner: root
- Scope: `crates/factory-core/src/codex.rs`, protocol tests/fixtures, `tests/manual/app-server-probe.md`
- Dependencies: Task 2
- Status: COMPLETE
- Acceptance: current installed protocol is confirmed; fragmented JSONL, output preservation, turn completion, interrupt, and exactly-once failure behavior are handled.
- Verification evidence: fake App Server tests passed for fragmented JSONL, output preservation, completion, interruption, exactly-once crash failure, oversized-line rejection, and process-group shutdown. The live adapter test passed twice on `codex-cli 0.157.0` in a disposable read-only Git repo; the latest run completed in 10.74 seconds, persisted real `agentMessage` text `hello` and `turn/completed`, and left no App Server process running.
- Commit: pending

## Task 4: Reconnect the read-only UI

- Owner: root
- Scope: `src/bridge.ts`, `src/App.tsx`, `src/styles.css`, `src-tauri/src/lib.rs`, UI tests, manual acceptance records
- Dependencies: Tasks 1, 2, and 3
- Status: IN_PROGRESS
- Acceptance: snapshot loads on reconnect, event gaps reload from the ledger, only disposable-repository turns can be started, and session output remains read-only and redacted.
- Verification evidence: the React bridge/App test passed snapshot restoration, output delivery, read-only UI, and snapshot reload on a sequence gap. `npm run build` passed. The Tauri dev process created the local SQLite ledger and marked disposable Git fixture, but native UI interaction could not be performed because the Mac was locked when the app was launched.
- Commit: pending
