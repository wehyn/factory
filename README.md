# Agentic Factory

Agentic Factory is a macOS desktop app for coordinating Codex work across registered Git
repositories. A resident Rust service owns the manager conversation, isolated run and agent
worktrees, scheduling, the event ledger, GitHub pull request gates, and post-merge production
observations. The React workspace shows runs, worktrees, live CLI sessions, handoffs, pull request
state, and production alerts.

## Run locally

```sh
npm install
npm run tauri dev
```

The app uses the installed `codex` CLI and App Server, `git`, the GitHub CLI (`gh`) for GitHub
actions, and `curl` for read-only production checks. Register a Git repository in the app; runs
and builders use separate Git worktrees. Creating a pull request explicitly publishes the run's
integration branch to the repository's `origin`. Automatic merging is disabled by default.

Repository-specific GitHub and production settings are optional in `.agentic-factory.json`.
See [repository configuration](docs/repository-config.md) for the merge policy, production
identity endpoint, smoke URL, timeouts, alert behavior, and security constraints.

## Build and verify

```sh
npm run build
npm test -- --run
cargo test --workspace
npm run tauri build
```

The end-to-end acceptance checklist is in [tests/e2e/factory-run.md](tests/e2e/factory-run.md).
The window and tray procedure is in [tests/manual/window-lifecycle.md](tests/manual/window-lifecycle.md).
The App Server protocol probe is in [tests/manual/app-server-probe.md](tests/manual/app-server-probe.md).

## Runtime behavior

- Closing the main window hides it. The service, sessions, ledger, and production watcher continue
  until the user quits the app.
- Quitting records the monitor stop. On the next launch, merged runs are marked unverified with a
  persistent monitoring-gap alert until a fresh production check completes.
- GitHub policy defaults to no auto-merge. The gate requires the current PR head, every configured
  required check, an independent approval for that exact head, a low-risk diff, and a valid
  production watch configuration. Create and merge actions use durable idempotency receipts.
- After a merged PR is observed, production watch compares the configured deployed commit SHA with
  the merge commit and checks a smoke URL. Missing evidence or a failed check creates a persistent
  alert and desktop notification. The watcher does not deploy, retry, or roll back; a user can
  request a fresh read-only check.
- SQLite stores session, run, worktree, PR, evidence, production observation, and alert state under
  the app's local data directory. Credentials and secret-like output are redacted before retention.

## Toolchain recorded for this branch

- macOS SDK 27.0; generated Mach-O minimum deployment target macOS 11.0.
- Rust and Cargo 1.96.0.
- Node.js 24.11.0 and npm 11.6.1.
- Tauri Rust crate 2.11.6, notification plugin 2.0.0, CLI 2.11.5, and JavaScript API 2.11.1.
- React and React DOM 19.3.0, Vite 8.3.1, and TypeScript 6.0.3.
- Codex CLI App Server probe: `codex-cli 0.157.0`.

`package-lock.json` and `Cargo.lock` pin resolved dependencies. This release targets macOS;
other platforms are outside the current plan. The release checklist distinguishes automated
fixture evidence from live GitHub and production observations; do not call the product
production-ready without those external checks.
