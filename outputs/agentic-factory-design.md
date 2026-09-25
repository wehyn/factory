# Agentic Software Factory — Design Spec

Status: Draft for Wayne's review · 26 September 2026

## Purpose

Build a standalone Tauri desktop app with a Rust orchestration service that coordinates Codex CLI agents on repository work. Wayne talks to one persistent manager agent across all repositories and tasks. The manager plans work, assigns bounded slices, integrates results, verifies the combined change, and delivers a pull request ready for review. Eligible low-risk pull requests may merge automatically after every required gate passes. The service follows merged changes into production; on a production failure, it alerts Wayne and waits for direction.

The app should make agent work observable without requiring Wayne to manage individual agents. Its main canvas shows the actual Codex CLI sessions and the relationships between them.

## User experience

### Factory home

The home view groups runs by repository and shows each run's owner, stage, PR, checks, linked runs, and worktrees. A run belongs to exactly one repository. Work that spans repositories becomes linked runs, each with its own integration branch, PR, checks, and production status. The manager coordinates dependencies across linked runs.

### One manager conversation

The manager has one persistent conversation that spans the factory. Wayne can refer to a repository or run in that conversation, and the manager confirms the active context when an instruction could apply to more than one task. All instructions to builders go through the manager. Builder CLI windows are read-only for Wayne; he can inspect their output, messages, decisions, files, and status.

### Run canvas

The canvas is an infinite, pannable workspace of live Codex CLI sessions. A manager session appears with the builder and verification sessions it spawned. Each window shows the agent, repository, worktree, task, process state, and live terminal output. Opening a window reveals its full output and evidence without replacing the canvas.

Connections have distinct meanings. A persistent directed connection records that the manager spawned and owns an agent. A message arrow appears when a real agent-to-agent handoff or update occurs; selecting it reveals sender, recipient, time, run, and message. Related tasks in different repositories are linked at the run level, not joined into one shared worktree. Workflow gates such as PR checks remain visible through run status and details, without a duplicate global CLI stream panel.

The current HTML prototype is a design demonstration with simulated content. The live product has no replay control and does not generate synthetic CLI activity to represent a real run.

## Run lifecycle

1. **Intake:** The manager identifies the target repository and desired outcome, inspects repository instructions and state, and records acceptance criteria and constraints.
2. **Mission plan:** The manager creates bounded slices with dependencies, ownership, file scope, and completion evidence. It parallelizes only independent, non-overlapping work. Sequential or shared-interface work waits for its prerequisite or an agreed contract.
3. **Isolation:** The service creates one integration worktree and branch for the run. Each coding agent receives its own worktree and branch, based on a recorded commit. The manager owns integration.
4. **Execution:** The scheduler starts Codex CLI processes. Their output streams to the canvas. Agents exchange messages through the service; important interface agreements and design decisions become durable records in the run ledger.
5. **Integration:** The manager checks ownership and dependencies, integrates slices, resolves conflicts, and verifies the combined result at the current commit. An independent review checks the complete diff before the PR gate.
6. **Pull request:** The manager opens a GitHub PR with a concise change summary, verification evidence, relevant decisions, and remaining limitations. GitHub is the first repository host. Agents may use `gh`; the service also tracks PR and check state so it does not depend on a running agent process for that information.
7. **Merge gate:** A run may merge only after all required checks on the current PR commit pass. Failed, pending, or missing required checks block merge. The conservative risk policy controls whether the manager may merge automatically or must wait for Wayne's review.
8. **Production watch:** After merge, the manager follows deployment and repository-defined production smoke checks. A healthy production result closes the run. A failure triggers an alert and a wait for Wayne; the service does not retry, roll back, or make another production change on its own.

## Manager, agent, and message contracts

The manager is accountable for the complete run, even when agents communicate directly. A builder receives a slice spec containing its objective, repository and worktree, allowed file scope, base commit, dependencies, acceptance criteria, and expected evidence. Its completion report lists changed commits or files, tests, decisions, unresolved issues, and any message sent to a peer.

Agent-to-agent communication goes through a service mailbox. Each message records sender, recipient, run, related slice, type, timestamp, and content. Types include question, answer, handoff, contract decision, blocker, and completion. The manager sees every message and resolves conflicts. A contract decision becomes part of the mission plan so dependent agents and reviewers can rely on the same version. Agents may talk to each other; they cannot silently take ownership of another agent's files or spawn new coding work outside the manager's plan.

The run ledger is the durable source of truth for plans, assignments, process IDs, worktrees, messages, decisions, commits, checks, PRs, and deployment observations. CLI transcripts are linked to the corresponding agent session. The UI reconstructs the canvas from this ledger and current process state after a restart.

## Service components

- **Tauri desktop UI:** A webview frontend for factory home, persistent manager chat, run canvas, worktree view, and PR/deployment status. It talks to the Rust service through a restricted local interface; it does not own the agents' process lifetimes.
- **Rust orchestration service:** A background runtime packaged with the Tauri app. It remains active when the window closes, supervises runs and production watches, and persists state. Reopening the window reconnects to the same runs. Explicitly quitting the service is a separate action from closing the window.
- **Manager runtime:** Codex CLI session with access to the run ledger and explicit actions for planning, delegation, integration, review, and PR decisions.
- **Rust scheduler:** Executes dependency-ready slices, enforces ownership, tracks agent state, and applies bounded retries.
- **Rust Codex worker runner:** Starts and supervises Codex CLI processes in isolated worktrees and streams their real output to the UI. It emits lifecycle events when processes start, exit, stall, or need input.
- **Message broker and event ledger:** Rust delivers agent messages, stores handoffs and decisions, and provides ordered events for live arrows and recovery. A local durable database stores run state; process output is retained with limits and redaction.
- **Rust Git/worktree manager:** Creates, inventories, protects, and cleans up worktrees and branches. It checks for uncommitted or unintegrated work before cleanup.
- **GitHub tracker:** Observes PRs, reviews, commit heads, required checks, merge state, and deployment signals. `gh` remains available for agent operations.
- **Policy and verification gate:** Evaluates risk, required checks, independent review, and merge eligibility against the current commit.
- **Production observer:** Uses repository-provided deployment identity and smoke checks. It reports `unverified` when it cannot establish production health.

The v1 release targets macOS. Its Rust service runs on the same Mac as Codex CLI, Git, and the target repositories. A single worker host is sufficient for v1; the interfaces should leave room for additional hosts later. The Tauri frontend is a client of that service, so closing or reopening its window does not interrupt an active run or production watch.

## Worktree rules

Each run has one integration worktree. Each agent that edits code has a separate worktree and branch. Agents never share a writable checkout. The manager records the base commit and checks whether the branch has moved before integrating. If two planned slices acquire overlapping file ownership, the manager stops the conflicting work and resolves ownership before either agent continues.

Worktrees remain visible while a run is active or under review. Cleanup runs only after the related work is integrated or intentionally abandoned and after checking for uncommitted changes. Linked runs in other repositories have independent worktrees and cleanup. The UI offers create, inspect, and archive actions, with the manager applying these rules.

## Conservative auto-merge policy

Auto-merge is available for documentation, comments, formatting, and small, well-tested code fixes that do not alter security, authentication, permissions, privacy, data migrations, deployment, secrets, or public interfaces. Repository-specific exclusions may make this policy stricter. New features, permission changes, and ambiguous risk classifications wait for Wayne's review.

For an eligible PR, the manager may merge only when the integrated change has passed final verification, independent review has no blocking finding, and every required GitHub check is successful on the PR's current commit. A new commit invalidates earlier check evidence until checks pass again. A failure, pending check, or absent required check prevents merging. The ledger records why the manager merged or held the PR.

## Production observation

Each repository supplies a small configuration identifying production deployments and meaningful smoke checks. The observer associates a deployment with the merged commit. A run is `healthy in production` only when that deployment and its required checks are confirmed. If production evidence is unavailable, the run is `production unverified` and Wayne is alerted. If a check fails, the manager alerts Wayne with the failing signal and commit, then waits. No automatic rollback or retry occurs.

## Recovery and safety

The ledger stores enough state to resume after the service or a CLI process crashes. The scheduler uses stable run, task, agent, and worktree IDs so restarts do not duplicate agents or PR actions. A stalled agent is retried within a bounded policy only after the prior process and worktree are accounted for; otherwise the manager surfaces a blocker. Cancellation stops processes before worktree cleanup.

Closing the Tauri window leaves the Rust service running. On reopening, the UI loads the ledger snapshot and resumes the live event stream. If the service itself stops or the machine restarts, it resumes active runs and production watches when launched again and reports any observation gap instead of claiming uninterrupted monitoring.

The service authenticates access to the app and limits repositories it may operate on. It avoids storing credentials in the ledger and redacts secrets from displayed or retained process output. Agent messages and tool output are treated as data; they do not override Wayne's instructions or the manager's run policy.

## v1 acceptance criteria

- One manager conversation can create and track concurrent runs in at least two repositories.
- Closing and reopening the Tauri window preserves active Codex CLI runs and production observation, and the UI reconnects to their current state.
- A linked pair of cross-repository tasks keeps separate branches, worktrees, PRs, and checks.
- The canvas shows real Codex CLI process output and adds agent windows when the manager spawns processes.
- A delivered agent message produces a correctly directed canvas arrow and an inspectable durable record.
- Builder windows are read-only to Wayne; his instructions reach builders through the manager.
- Independent builder work uses separate worktrees; overlap is detected before integration.
- A full disposable-repository run reaches a PR with combined verification and independent review evidence.
- Failed, pending, or missing required checks prevent merge. A current, passing, low-risk PR can auto-merge; a permissions change waits for review.
- The service survives a restart without duplicating an agent, worktree, PR, or merge action.
- Production failure alerts Wayne and waits, with no automatic rollback. Missing deployment evidence is reported as unverified.

## Outside v1

Additional coding CLIs, non-GitHub repository hosts, Linux or Windows packaging, distributed worker hosts, autonomous agent-created task trees, and automatic production rollback are outside the first release.
