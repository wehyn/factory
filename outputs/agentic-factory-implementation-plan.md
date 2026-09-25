# Agentic Factory Implementation Plan

> **For implementation:** Execute the checked tasks in order, with a review and verification pass before each dependent task. Delegate only independent, bounded work with separate file ownership.

**Goal:** Build a macOS Tauri app that lets Wayne manage concurrent repository runs through one Codex manager, observe real Codex CLI sessions and messages on a canvas, and deliver verified PRs with conservative auto-merge and production monitoring.

**Architecture:** A resident Tauri 2 process owns a Rust orchestration core and a React/TypeScript UI. Rust supervises `codex app-server` processes over stdio, persists an event ledger, owns worktrees and policy decisions, and emits snapshots plus ordered events to the UI. The UI renders the actual Codex session events as read-only terminal-like windows on a React Flow canvas. Closing the window hides it while the Rust process continues; explicit Quit stops it after reporting active work.

**Tech Stack:** macOS, Rust, Tauri 2, TypeScript, React, Vite, Vitest, Testing Library, `@xyflow/react`, SQLite, Codex CLI App Server, Git, GitHub CLI.

**Spec:** [agentic-factory-design.md](./agentic-factory-design.md)

## Global Constraints

- V1 targets macOS and one worker host. No Linux or Windows packaging in this plan.
- Wayne has one persistent manager conversation across all repositories and runs. Builder sessions are read-only in the UI.
- One run belongs to one repository; cross-repository work uses linked runs with separate PRs and checks.
- Every editing agent has an exclusive worktree and branch. The manager owns integration.
- No merge while a required check on the current PR commit is failed, pending, or missing.
- Conservative low-risk auto-merge excludes security, authentication, permissions, privacy, migrations, deployment, secrets, and public interfaces.
- Production failure alerts Wayne and waits. No automatic retry or rollback.
- The real app has no replay control or duplicate global CLI stream panel. The HTML prototype is visual reference only.
- Keep credentials out of the ledger and redact secret-like output before retention or display.

## Execution boundary and source references

This plan describes a **new Git repository**. All paths below are relative to its root; the current projectless design folder is not the product repository. Before Task 1, choose the repository location and record the exact path in the new repository README. Do not copy the prototype's simulated run data into production fixtures.

Use the installed CLI's generated App Server schema as the protocol source of truth: `codex app-server generate-json-schema --out work/codex-schema`. The planning baseline observed on this Mac is Codex CLI `0.157.0`, Rust `1.96.0`, Node `24.11.0`, and `gh` `2.101.0`; recheck at execution. The [official Codex App Server guide](https://learn.chatgpt.com/docs/app-server) documents stdio JSONL, `initialize`, thread and turn methods, notifications, and schema generation. The [Tauri sidecar guide](https://v2.tauri.app/develop/sidecar/) and [React Flow custom edge guide](https://reactflow.dev/examples/edges/custom-edges) are implementation references. Avoid experimental App Server process APIs and network WebSocket transport for v1.

## File map

| Path | Responsibility |
| --- | --- |
| `src-tauri/src/lib.rs` | Tauri setup, window hide/reopen, tray/menu, typed commands and event bridge |
| `src-tauri/src/main.rs` | Desktop entry point only |
| `crates/factory-core/src/model.rs` | IDs, run/task/agent states, message and gate types |
| `crates/factory-core/src/ledger.rs` | SQLite schema, append-only events, snapshots and recovery |
| `crates/factory-core/src/codex.rs` | Stdio App Server client and typed notification adapter |
| `crates/factory-core/src/repositories.rs` | Repository registry and base-commit inspection |
| `crates/factory-core/src/worktrees.rs` | Worktree creation, inventory, isolation and safe cleanup |
| `crates/factory-core/src/mailbox.rs` | Durable directed agent messages and delivery state |
| `crates/factory-core/src/mcp.rs` | Factory MCP tools used by manager/builders during turns |
| `crates/factory-core/src/scheduler.rs` | Dependency graph, worker lifecycle, retry and integration readiness |
| `crates/factory-core/src/github.rs` | `gh` adapter, PR/head/check observation and guarded merge |
| `crates/factory-core/src/policy.rs` | Conservative risk and merge eligibility rules |
| `crates/factory-core/src/production.rs` | Deployment identity, smoke checks and waiting state |
| `src-tauri/src/notifications.rs` | Persistent in-app alerts and macOS notifications |
| `src/bridge.ts` | Typed frontend calls and snapshot/event subscription |
| `src/FactoryHome.tsx` | Repository and run navigation, linked runs, worktree inventory |
| `src/ManagerChat.tsx` | One persistent manager conversation |
| `src/RunCanvas.tsx` | Live Codex session nodes and spawn/message edges |
| `src/AgentNode.tsx` | Read-only live output, worktree and process state |
| `src/MessageEdge.tsx` | Directed, inspectable handoff arrow |
| `src/styles.css` | Responsive desktop layout and visual hierarchy |

## Milestone 1 — A resident app with one real Codex session

### Task 1: Bootstrap Tauri shell and resident lifecycle

**Files:** Create `package.json`, `src-tauri/Cargo.toml`, `src-tauri/src/{main,lib}.rs`, `src/App.tsx`, `src/styles.css`, `README.md` through the Tauri React/TypeScript scaffold. Modify generated files only as required.

**Interfaces:** Produces `run()` in `src-tauri/src/lib.rs` and a `show_main_window` command. Later tasks add shared Rust state and UI views without changing this lifecycle contract.

- [ ] Scaffold a Tauri 2 React/TypeScript project with `npm create tauri-app@latest`; initialize Git, commit the generated baseline, and record the selected app identifier and minimum macOS version in README.
- [ ] Add a focused lifecycle test or debug harness that opens the app, closes the main window, confirms the process remains alive, then reopens from the tray/menu. Record the observed PID and window state; this is the acceptance surface for background monitoring.
- [ ] In `lib.rs`, intercept `WindowEvent::CloseRequested`, call `api.prevent_close()`, hide the window, and provide a tray/menu action that shows and focuses it. Add explicit Quit plumbing; Task 9 adds active-run and monitoring-gap reporting. Keep the frontend webview incapable of spawning arbitrary shell commands.
- [ ] Run `npm run build`, `cargo test --workspace`, and `npm run tauri dev`; repeat the close/reopen check on macOS. Commit as `feat: add resident Tauri shell`.

### Task 2: Durable run ledger and recovery

**Files:** Create root `Cargo.toml`, `crates/factory-core/Cargo.toml`, `crates/factory-core/src/{lib,model,ledger}.rs`, `crates/factory-core/tests/ledger_recovery.rs`; add the Tauri crate and core crate to the root Cargo workspace.

**Interfaces:** Produces `Ledger::append(event: Event) -> Result<i64>`, `Ledger::snapshot() -> Result<FactorySnapshot>`, and stable `RepoId`, `RunId`, `AgentId`, `MessageId` newtypes. Later modules append events rather than mutating UI state directly.

- [ ] Define the event envelope with `event_id`, `run_id`, `agent_id`, `kind`, `payload`, and `created_at`; model run states from `intake` through `production_healthy`, `production_unverified`, and `needs_attention`. Store events in SQLite with a unique event ID and monotonically increasing sequence.
- [ ] Write a recovery test: append `RunCreated`, `AgentSpawned`, and `MessageSent`, drop the database handle, reopen it, and assert the same snapshot and message order. A duplicate event ID must leave one row.
- [ ] Implement migrations, append transaction, snapshot projection, and bounded output retention. Persist an output segment only after redaction; never store environment variables or Codex credentials.
- [ ] Run `cargo test -p factory-core ledger_recovery` and the full crate tests; commit as `feat: persist factory run events`.

### Task 3: Prove and implement the Codex App Server adapter

**Files:** Create `crates/factory-core/src/codex.rs`, `crates/factory-core/tests/codex_protocol.rs`, and a disposable test repository fixture under `tests/fixtures/repo`.

**Interfaces:** Produces `CodexSession::start(cwd: PathBuf, role: AgentRole)`, `start_turn(input: String)`, `steer(input: String)`, `interrupt()`, and a stream of typed `CodexEvent` values. Stores the App Server thread ID in the ledger.

- [ ] Generate JSON Schema from the installed Codex CLI. Build a tiny read-only protocol probe that spawns `codex app-server` over stdio, sends `initialize` and `initialized`, starts a thread, runs a harmless turn in the disposable repo, and captures `item/*` and `turn/completed` notifications. Confirm that the UI can display the real event stream. Do not run coding prompts against a user repository for this probe.
- [ ] Add a fake stdio server test that fragments JSONL across reads, interleaves response IDs with notifications, and exits mid-turn. Assert that the adapter emits one terminal failure and preserves already received output. This tests the process boundary rather than mirroring parsing code.
- [ ] Implement request ID correlation, bounded line/output buffers, stderr capture, thread resume, cancellation, and typed notification mapping. Use the generated schema for the observed CLI version; reject unknown state-changing responses while retaining unknown display-only events as diagnostics.
- [ ] Run `cargo test -p factory-core codex_protocol`, then repeat the harmless live probe. Commit as `feat: supervise real Codex sessions`. If the installed CLI cannot supply stable turn and item events, stop here and revise the architecture before later tasks.

## Milestone 2 — Isolated multi-agent work and live canvas

### Task 4: Repository registry and exclusive worktrees

**Files:** Create `crates/factory-core/src/{repositories,worktrees}.rs`, `crates/factory-core/tests/worktree_isolation.rs`.

**Interfaces:** Produces `create_run_worktree(repo_id, run_id, base_sha) -> Worktree`, `create_agent_worktree(run_id, agent_id, base_sha) -> Worktree`, `inspect_worktree(id) -> WorktreeStatus`, and `archive_worktree(id) -> Result<ArchiveOutcome>`.

- [ ] Write a test with a temporary Git repository: create two agent worktrees from the same base, edit distinct files, and confirm changes stay isolated. Leave an uncommitted file in one worktree and assert archive refuses it without deleting data.
- [ ] Implement repository registration by canonical Git root, store remote and default branch, and record the exact base SHA before creating a run. Use `git worktree add -b` with argument arrays, never a shell-interpolated command string. Give the manager a separate integration branch/worktree.
- [ ] Add ownership reservations for writable file scopes. A second active agent claiming the same path must receive a conflict state; the manager must resolve ownership before continuation.
- [ ] Run `cargo test -p factory-core worktree_isolation` and inspect a real temporary worktree list; commit as `feat: isolate agent worktrees`.

### Task 5: Manager delegation, mailbox, and agent tools

**Files:** Create `crates/factory-core/src/{mailbox,mcp}.rs`, `crates/factory-core/tests/message_delivery.rs`, and `docs/agent-contracts.md`.

**Interfaces:** Produces durable `send_message(from, to, run_id, kind, body) -> MessageId` and `deliver_pending(agent_id) -> Vec<Message>`. The manager-only action `assign_slice` validates run ownership, dependency IDs, file scope, and worktree availability before Task 6 starts a worker.

- [ ] Document the manager and builder contracts: slice objective, repo/worktree, base SHA, allowed paths, dependencies, acceptance evidence, and message types (`question`, `answer`, `handoff`, `contract`, `blocker`, `completion`). Wayne messages only the manager; builders cannot send to Wayne or start unowned coding work.
- [ ] Prove a per-session factory MCP tool is callable from a Codex App Server turn using an isolated configuration override. Provide `send_message` to workers and `assign_slice` to the manager; the Rust core remains the authority and validates every tool call. Keep the user's global Codex config and credentials unchanged.
- [ ] Test directed delivery with two agents: a message from A to B appears once in B's inbox, appears in the manager ledger, and does not enter C's inbox. A contract message becomes a versioned run decision; a conflicting contract pauses dependent work.
- [ ] Implement mailbox persistence, acknowledgements, and manager visibility. On active recipient turns, use supported turn steering only after verifying delivery semantics; otherwise queue for the recipient's next turn. Run the protocol and mailbox tests; commit as `feat: route agent handoffs`.

### Task 6: Dependency scheduler and integration gate

**Files:** Create `crates/factory-core/src/scheduler.rs`, `crates/factory-core/tests/scheduler_recovery.rs`.

**Interfaces:** Produces `Scheduler::ready(run_id) -> Vec<SliceId>`, `spawn_slice(slice_id) -> Result<AgentId>`, `complete_slice(agent_id, evidence)`, and `integration_ready(run_id) -> bool`.

- [ ] Test a graph with API and UI in parallel and verification dependent on both. Verification must not start after only one completion. Simulate a process crash and reload the ledger; the scheduler must not spawn a duplicate agent for the same slice.
- [ ] Implement stable task IDs, dependency checks, active ownership checks, bounded retry decisions, and process termination accounting. A conflicting path reservation or uncertain prior process stops the slice and surfaces a manager blocker.
- [ ] Make the manager integrate only completed slices into the integration worktree, recording the exact source commits and destination commit. Final verification and independent review run against that integrated commit, not separate builder branches.
- [ ] Run `cargo test -p factory-core scheduler_recovery` and a two-worker disposable-repo scenario; commit as `feat: schedule bounded agent work`.

### Task 7: Factory home, persistent chat, and live CLI canvas

**Files:** Create `src/bridge.ts`, `src/{FactoryHome,ManagerChat,RunCanvas,AgentNode,MessageEdge}.tsx`, `src/styles.css`, `src/RunCanvas.test.tsx`; modify `src/App.tsx` and `src-tauri/src/lib.rs`.

**Interfaces:** Rust exposes `get_snapshot() -> FactorySnapshot` and emits `{sequence, event}`. The frontend applies events in sequence; a gap triggers snapshot reload. `RunCanvas` receives one run and read-only agent output segments.

- [ ] Install and configure Vitest and Testing Library. Add a UI behavior test using fixture events: `AgentSpawned` creates a node, `MessageSent(A,B)` creates a directed inspectable arrow, and a sequence gap reloads the snapshot. The test should assert visible behavior and message provenance, not React component internals.
- [ ] Implement the factory home from the approved prototype: repositories, concurrent runs, linked cross-repo tasks, PR/check state, and worktree inventory. The same manager chat persists while switching runs and shows the active context.
- [ ] Use `@xyflow/react` custom agent nodes and message edges for pan/zoom, drag, selection and arrow direction. Stream real App Server output into each read-only node with bounded rendering; clicking a message opens its sender, recipient, time, type and text. There is no replay button or duplicate bottom stream panel.
- [ ] Run `npm test`, `npm run build`, and `npm run tauri dev`. Inspect the actual app at a normal laptop viewport; compare visual hierarchy and canvas interaction with the approved HTML prototype. Commit as `feat: show live agent sessions on canvas`.

## Milestone 3 — PR delivery and production watch

### Task 8: GitHub PR tracking and conservative merge gate

**Files:** Create `crates/factory-core/src/{github,policy}.rs`, `crates/factory-core/tests/merge_gate.rs`, `docs/repository-config.md`.

**Interfaces:** Produces `observe_pr(repo, number) -> PullRequestState`, `classify_risk(diff, repo_policy) -> RiskDecision`, and `merge_decision(run, pr, review, checks) -> MergeDecision { AutoMerge, WaitForReview, Block(reason) }`.

- [ ] Write a gate test matrix for current-head checks: all green + independently reviewed + low risk permits auto-merge; any failed, pending, missing, or stale-head check blocks; permissions or migration changes wait for Wayne even when tests pass. A new commit invalidates an earlier green result.
- [ ] Implement `gh` calls with argument arrays for PR creation, head SHA, reviews and check state. Store idempotency keys for create/merge attempts; re-read PR head and required checks immediately before issuing a merge. The Rust policy engine, not a Codex text response, makes the final merge decision.
- [ ] Generate PR bodies from the ledger: change summary, verification commands/results, independent review, decisions, limitations, and worktree/commit provenance. Keep review-required PRs open until Wayne acts.
- [ ] Test against a disposable GitHub repository or a recorded `gh` fixture plus one live read-only PR check; do not claim live merge coverage from a fixture. Run `cargo test -p factory-core merge_gate`; commit as `feat: gate GitHub merges`.

### Task 9: Production deployment observation and alerts

**Files:** Create `crates/factory-core/src/production.rs`, `crates/factory-core/tests/production_watch.rs`, `src-tauri/src/notifications.rs`; update `docs/repository-config.md` and the factory home status display.

**Interfaces:** Produces `observe_deployment(repo, merged_sha) -> DeploymentObservation` and `evaluate_health(observation, config) -> Healthy | Failed(reason) | Unverified(reason)`. Alerts persist until Wayne acknowledges or gives a new instruction.

- [ ] Define per-repository configuration for the production environment identifier, merged SHA matching rule, check source, smoke command or URL, and timeout. Persist alerts in the app and send macOS notifications when attention is required. Validate the config before an auto-merge can claim that production babysitting is available.
- [ ] Test three outcomes using fake deployment events: matching SHA + passing smoke check marks healthy; failing check records an alert and does not call rollback/retry; absent deployment evidence marks unverified and alerts. Reopen the ledger after each outcome and confirm the state survives restart.
- [ ] Implement polling or event-driven observation with bounded retries for reads only. Production-affecting actions remain under Wayne's direction. Closing the Tauri window must not stop the Rust watcher; explicit Quit reports active runs and that monitoring will stop.
- [ ] Run `cargo test -p factory-core production_watch` and a local fake deployment end-to-end. Commit as `feat: watch production after merge`.

### Task 10: Full acceptance run and release readiness

**Files:** Create `tests/e2e/factory-run.md` and only the automation needed to make the acceptance run repeatable; update `README.md` and user-facing error copy.

**Interfaces:** Exercises the public UI and service boundaries from the spec; no new internal interface.

- [ ] In two disposable repositories, start concurrent runs through the one manager conversation and link one task across repos. Confirm separate integration/agent worktrees, separate PRs/checks, real CLI windows, and directed handoff arrows.
- [ ] Exercise the guarded paths: failed check blocks merge; permissions change waits for review; low-risk passing change auto-merges; production failure alerts without rollback; missing production evidence shows unverified. Capture the actual PR heads, check states and deployment observations used for each conclusion.
- [ ] Close/reopen the Tauri window during an active run and production watch. Restart the Rust process in a controlled disposable run; confirm event recovery, no duplicate agent or PR action, and explicit reporting of any monitoring gap.
- [ ] Run `cargo test --workspace`, `npm test`, `npm run build`, `npm run tauri build -- --debug`, and the end-to-end checklist. Review the complete diff, fix findings, and commit release documentation. Do not call the product production-ready until a real Codex session, GitHub gate, and production observation are evidenced.

## Plan self-review

- **Spec coverage:** Tasks 1–3 cover resident Tauri/Codex runtime and recovery; 4–6 cover worktrees, delegation, messages and integration; 7 covers home, global chat and live canvas; 8 covers PR/review/merge policy; 9 covers production observation; 10 verifies the combined run.
- **Critical proof points:** Task 3 validates the installed App Server protocol before building on it. Task 5 validates mid-turn message tooling before claiming swarm handoffs. Task 8 validates required checks against the current PR head before any merge decision.
- **Execution order:** Complete and review each task before the next task that consumes its interface. Milestones produce a runnable app with increasing capability; do not parallelize overlapping Rust core files or shared frontend state.
