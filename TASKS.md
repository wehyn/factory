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

## Remaining v1 roadmap tasks

The roadmap IDs below refer to `outputs/agentic-factory-implementation-plan.md`; they are distinct from the resident-slice task numbers above.

### Roadmap Task 4: Repository registry and exclusive worktrees (R4)

- Owner: root
- Scope: `crates/factory-core/src/{model,ledger,repositories,worktrees,lib}.rs`, `crates/factory-core/tests/worktree_isolation.rs`, Tauri service wiring where required
- Dependencies: resident ledger and App Server adapter (complete)
- Status: COMPLETE
- Acceptance: register canonical Git roots and record remote/default branch; create a run integration worktree and exclusive agent worktrees from exact base SHAs; detect overlapping file ownership; refuse unsafe or dirty archival without data loss; persist worktree metadata and recovery issues.
- Verification evidence: `cargo test -p factory-core` passed (28 passed, one ignored); the worktree isolation suite passed five consecutive concurrent runs; `cargo check -p agentic-factory`, `cargo fmt --all -- --check`, and `git diff --check` passed. Coverage verifies Git's real worktree list, exact base SHA, isolated agent edits, credential scrubbing, reservation conflicts across SQLite connections, archive refusal/preservation, symlink and missing-path recovery, and startup reconciliation around a live creator.
- Branch: `feat/repository-worktrees`
- Commit: a363a83

### Roadmap Task 5: Manager delegation, mailbox, and agent tools (R5)

- Owner: root
- Scope: `crates/factory-core/src/{mailbox,mcp}.rs`, Codex tool configuration, `crates/factory-core/tests/message_delivery.rs`, `docs/agent-contracts.md`
- Dependencies: R4 identity, worktree, and ownership contracts
- Status: COMPLETE
- Acceptance: manager-only assignment validates run, dependencies, file scope, and worktree; directed messages persist and deliver once; contract decisions are versioned and conflicts stop dependent work; a harmless App Server turn invokes scoped factory MCP tools without mutating global Codex configuration.
- Verification evidence: `cargo test -p factory-core` passed (33 passed, two live tests ignored by default); the scoped MCP App Server probe passed explicitly on `codex-cli 0.157.0` and confirmed global Codex config metadata was unchanged. `cargo check --workspace`, `cargo fmt --all -- --check`, and `git diff --check` passed. Message delivery, acknowledgements, manager-only assignment, dependency/scope validation, contract conflict pausing/resolution, principal-specific tool exposure, and stdio handshake are covered.
- Commit: d152233 (`feat: route agent handoffs`)

### Roadmap Task 6: Dependency scheduler and integration gate (R6)

- Owner: root
- Scope: `crates/factory-core/src/scheduler.rs`, `crates/factory-core/tests/scheduler_recovery.rs`
- Dependencies: R4 and R5
- Status: COMPLETE
- Acceptance: independent slices schedule concurrently, dependent slices wait for every dependency, restart cannot duplicate a slice, retries are bounded, and manager integration records source and destination commits in the integration worktree.
- Verification evidence: `cargo test -p factory-core` passed (40 passed, three live tests ignored by default); `cargo check --workspace`, `cargo fmt --all -- --check`, and `git diff --check` passed. The explicit two-worker disposable-repository Codex probe passed: both real App Server workers edited disjoint files, sent completion evidence through Factory MCP, and the service recorded and integrated their scoped commits. Scheduler tests cover dependency gating, restart blocking, bounded retry, scope enforcement, and source/destination SHA records.
- Commit: 87cb6f0 (`feat: schedule bounded agent work`)

### Roadmap Task 7: Factory home, persistent chat, and live CLI canvas (R7)

- Owner: root
- Scope: frontend bridge and workspace/canvas components, `package.json`, `src/styles.css`, Tauri home/chat commands, durable chat migration, App Server multi-turn support, and UI/core recovery tests
- Dependencies: R4–R6
- Status: COMPLETE
- Acceptance: home shows repositories/runs/worktrees/gates; one manager chat persists across run selection; real read-only agent sessions render on a pannable canvas; directed message arrows expose provenance; sequence gaps restore the snapshot.
- Verification evidence: `cargo test -p factory-core` passed (44 passed, five live tests ignored); `npm test -- --run` passed (three UI behavior tests); `npm run build`, `cargo check --workspace`, `cargo fmt --all -- --check`, and `git diff --check` passed. The real Tauri development binary compiled and launched against the existing local data; its older installed app has the same macOS bundle identity, so native viewport inspection is recorded for R10's isolated release acceptance.
- Commit: f569cc6 (`feat: show live agent sessions on canvas`)

### Roadmap Task 8: GitHub PR tracking and conservative merge gate (R8)

- Owner: root
- Scope: `crates/factory-core/src/{github,policy}.rs`, `crates/factory-core/tests/merge_gate.rs`, `docs/repository-config.md`
- Dependencies: R6 integrated commits and review evidence
- Status: IMPLEMENTED; live GitHub acceptance pending
- Acceptance: `gh` observation and idempotent PR actions; current-head required-check revalidation immediately before merge; risk policy blocks security, permission, migration, deployment, secret, privacy, and public-interface changes from auto-merge; PR bodies include durable evidence.
- Verification evidence: fixture-backed PR creation, durable idempotency receipts, required-check parsing, current-head revalidation, risk policy, evidence-rich PR bodies, and integration-branch binding are covered by `cargo test --workspace`. A mismatched PR number is rejected without occupying the run's tracked-PR slot; a subsequent intended PR can still be created. `origin` now points to the empty public project repository `wehyn/factory`; live gate acceptance still needs separate disposable repositories and their branch/check/review setup.

### Roadmap Task 9: Production observation and alerts (R9)

- Owner: root
- Scope: `crates/factory-core/src/production.rs`, Tauri notifications/runtime, `docs/repository-config.md`, production-watch tests and UI
- Dependencies: R8 merged-commit/deployment identity
- Status: IMPLEMENTED; live production acceptance pending
- Acceptance: matching deployment and passing smoke check becomes healthy; failure persists an alert and waits without rollback/retry; missing evidence becomes unverified and alerts; state survives restart and window hiding.
- Verification evidence: fake-HTTP identity and smoke outcomes, durable alerts/acknowledgements, restart recovery, and the no-automatic-retry/rollback behavior are covered by `cargo test --workspace`. Live deployment identity and smoke checks remain pending because no production test environment is configured.

### Roadmap Task 10: Full acceptance and release readiness (R10)

- Owner: root
- Scope: `tests/e2e/factory-run.md`, README, user-facing errors, app/runtime acceptance
- Dependencies: R4–R9
- Status: LOCAL VERIFICATION COMPLETE; full acceptance pending
- Acceptance: two disposable repositories exercise concurrent linked runs, isolated workers, real Codex output, GitHub gate outcomes, production watch, window close/reopen, and controlled restart without duplicate actions; release checks and evidence are recorded.
- Verification evidence: the full workspace Rust suite, frontend tests/build, and regular plus isolated-preview Tauri debug bundles are verified locally. The three normally ignored App Server/MCP/parallel-worker integration cases were run individually and passed. The isolated preview app was visually reviewed at 2160×1440 without replacing the installed app. Two-repository GitHub and production-watch acceptance, plus active-run close/reopen/restart acceptance against the final bundle, remain pending; see `tests/e2e/factory-run.md`.
