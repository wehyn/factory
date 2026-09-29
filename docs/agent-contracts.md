# Manager and builder contracts

Agentic Factory has one accountable manager. Wayne gives instructions to the manager; builders receive scoped assignments from the manager and return questions, handoffs, blockers, contract proposals, and completion evidence through the durable mailbox.

## Manager responsibilities

The manager owns the plan, dependencies, integration worktree, contract decisions, review, pull request, merge decision, and production follow-up. Before starting a builder, the manager creates an assignment with:

- a stable assignment key and one concrete objective;
- the run, repository, integration base SHA, and exclusive worker worktree;
- an explicit list of writable paths;
- dependency slice IDs and any shared contract keys;
- evidence that will demonstrate completion.

`factory_assign_slice` is available only to a manager MCP principal. Rust checks that the run is active, every dependency belongs to the same run, the integration worktree is available, and the requested paths do not overlap another active owner. Retrying the same key with the same slice content returns the existing assignment.

The scheduler starts independent queued slices in parallel. A dependent slice waits until every prerequisite has been committed and integrated, then its clean worker worktree advances to the integration commit so it can build and verify against those changes. A restart never guesses whether a running worker is alive: it marks the slice blocked for manager review. A failed attempt can be retried once only when its process is accounted for and its worktree is clean at its recorded base.

## Builder responsibilities

A builder works only in its assigned worktree and allowed paths. Its Codex turn gets workspace-write permission for that worktree alone, with network access disabled. Git metadata lives in a protected shared directory, so the service validates the changed paths and records the scoped edits as a source commit after the completed turn. The builder does not create work, assign another builder, expand its scope, merge, or send instructions to Wayne. It can read its inbox, acknowledge directed messages, and send a question, answer, handoff, contract proposal, blocker, or completion report to the manager (or a directed peer where the manager's protocol permits it).

The MCP server binds identity from the process configuration. Tool arguments cannot select a different principal or run. Every operation is authorized in the Rust handler, even when a caller invokes a tool name that is absent from its advertised tool list.

## Message and contract behavior

Messages are durable run records. A directed worker message appears in exactly one recipient inbox and in the manager's run ledger. It remains available until acknowledged; reading it again does not create a duplicate. If a worker is not in an active turn, delivery waits for its next inbox read.

Contract proposals are versioned by run and key. Conflicting proposals pause assignments that declare that key. The manager resolves the contract explicitly; dependent assignments return to the waiting or queued state only after the decision is recorded. Builders read the authoritative decisions attached to their assignment with `factory_get_contracts`.

## Completion and integration

A builder reports completion evidence through the mailbox. The manager records completion with `factory_complete_slice`; Rust requires a completed App Server turn, validates every tracked and untracked change against the assigned paths, and records a source commit descended from the worker's base. Only then can the manager integrate the source commit range into the integration worktree. Each integration record stores the worker base SHA, source SHA, integration-before SHA, and destination SHA. Interrupted or conflicting integration is left for manager recovery and is never replayed automatically.

## App Server configuration

The factory MCP server receives the ledger path, worktree root, and process-bound principal through a per-session Codex App Server config overlay. The overlay is passed as command-line `--config` values and is not written to the user's global Codex configuration. The MCP process emits protocol JSON only on stdout; diagnostics go to stderr.
