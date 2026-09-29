# Agentic Factory acceptance run

This checklist separates repeatable fixture proof from live-service proof. Do not mark a live
step complete based on a fake CLI, unit test, or a code review.

## Evidence record

- Date and operator: 2026-09-29 / Codex local run
- App build / commit: `Agentic Factory Preview` debug bundle (`dev.wayne.agenticfactory.preview`), source commit `99528a4` pushed to `wehyn/factory` `main`
- macOS version: 27.0
- Codex CLI version: 0.157.0
- GitHub CLI version and authentication status (never record credentials): 2.101.0; authenticated to github.com (credentials omitted)
- Disposable repository A and commit: not selected; `wehyn/factory` is the public project repository, not a disposable test target
- Disposable repository B and commit: not selected; `wehyn/factory` is the public project repository, not a disposable test target
- Production test environment ID: not configured
- Screenshots or log paths: `/tmp/agentic-factory-preview-final.png`; bundle at `target/debug/bundle/macos/Agentic Factory Preview.app`

## Prepare disposable repositories

- [ ] Create two empty disposable repositories with distinct GitHub remotes and a committed
  `main` branch. Do not point the app at a user project.
- [ ] Register both repositories in Agentic Factory. Confirm their canonical roots and remotes.
- [ ] Add a valid `.agentic-factory.json` to each repository. Use distinct `production.environment_id`
  values, HTTPS deployment identity URLs, a smoke URL, and a short timeout.
- [ ] Configure GitHub checks and the docs-only auto-merge allowlist on the repository used for
  low-risk merge acceptance. Leave automatic merging disabled in any other repository.
- [ ] Start the app and record the app PID and manager session ID.

## Concurrent linked runs and worker isolation

- [ ] Create one run in each repository and link the two runs from the workspace.
- [ ] Ask the single manager conversation to split the work into non-overlapping changes in both
  repositories. Confirm separate integration worktrees and exclusive builder worktrees.
- [ ] Start independent slices in both runs. Confirm both live CLI sessions appear on the canvas,
  each builder's output and branch belong to its run, and a directed handoff arrow shows the
  sender and recipient.
- [ ] Confirm workers produce real Codex output and completion evidence. Record the run IDs,
  worktree paths, branch names, source commit SHAs, integration SHAs, and App Server thread IDs.
- [ ] Refresh the app snapshot and confirm the same sessions, messages, and worktree inventory are
  restored without starting duplicate workers.

## GitHub gate and durable action receipts

Run these cases on disposable pull requests; record the PR number and head SHA for every case.

- [ ] Refresh a PR with a pending or failed required check. The gate blocks it.
- [ ] Push a new commit after checks pass. Refresh the PR; stale checks and an approval from the
  previous head cannot authorize a merge.
- [ ] Change a permission, migration, deployment/workflow, secret, privacy, security, or public API
  surface. The gate waits for Wayne even when checks pass.
- [ ] Make a small docs-only change with every configured check passing and an independent review
  approval on the current head. Confirm the UI shows the eligible merge action.
- [ ] Click **Merge with current checks** once. Confirm the merge command includes the recorded
  head SHA, then refresh and verify the merged commit SHA.
- [ ] Repeat the same create/merge action key through a fixture or controlled retry. Confirm the
  ledger returns the original receipt and GitHub receives no duplicate action.
- [ ] Create a pull request from an integrated run. Confirm the app publishes only that run's
  integration branch and the PR body includes stored verification, review, decision, limitation,
  and worktree/commit provenance.

## Production watch

- [ ] For a merged disposable PR, return its exact merge SHA from the test identity URL and HTTP
  2xx from the smoke URL. Confirm the production gate becomes healthy for the configured
  environment.
- [ ] Change the identity response to a different full commit SHA. Confirm the gate waits for the
  matching deployment and persists an alert.
- [ ] Restore the matching SHA and return HTTP 503 from the smoke URL. Confirm a failure alert is
  persisted and a desktop notification is issued. Confirm no deploy, retry, or rollback action is
  made automatically.
- [ ] Remove identity or smoke evidence. Confirm the run becomes unverified and alerts.
- [ ] Acknowledge an alert, close and reopen the window, then restart the app. Confirm the alert
  and acknowledgement survive and a clean-quit monitoring gap is reported as unverified until a
  fresh check completes.
- [ ] Click **Run production check** to request a new observation. Confirm healthy state resolves
  the prior alert without deleting its history.

## Window and process lifecycle

- [ ] Start an active run and a merged-run production watch. Close the main window; confirm the
  window hides while the app PID and worker state remain active. Reopen from the tray and confirm
  the same run/session state appears.
- [ ] Quit the app from the tray. Confirm the Rust service and supervised children exit, and the
  persistent ledger records that monitoring stopped.
- [ ] Relaunch the app. Confirm event and alert recovery, a visible report of any monitoring gap,
  and no duplicate agent, PR, or merge action.

## Release verification

Run from the repository root:

```sh
cargo fmt --all -- --check
cargo test --workspace
npm test -- --run
npm run build
npm run tauri build -- --debug
git diff --check
```

- [x] Review the isolated preview app and rendered workspace at 2160×1440. Its bundle ID is `dev.wayne.agenticfactory.preview`; the installed `Agentic Factory` app was left untouched.
- [ ] Record every live PR head/check state and deployment observation used above.
- [x] List skipped steps and why. This product is not production-ready until the real Codex,
  GitHub, production watch, and process-lifecycle evidence above has been recorded.

## Current checkout evidence

This implementation run verifies the merge path against a fake `gh` executable, production
outcomes against a fake `curl` executable, durable SQLite recovery, workspace builds, and a
packaged Tauri build. The isolated preview app was reviewed at 2160×1440. The three normally
ignored App Server/MCP/parallel-worker integration cases were run individually and passed.

Skipped live steps and why:

- Two disposable repositories, real PR check/merge cases, and duplicate-action retries: `origin`
  is configured to the public project repository `wehyn/factory`, which is not a disposable test
  target. No disposable repositories were selected, and no live PR or merge was performed.
- Matching deployment identity, failing smoke check, and live alert recovery: no production test
  environment or identity/smoke endpoints are configured.
- Active-run close/reopen/restart against the final packaged bundle with production monitoring:
  local lifecycle primitives have prior evidence, but this combined release acceptance has not
  been run.

The live GitHub, production-watch, and combined process-lifecycle boxes above remain unverified;
the product is not production-ready until those steps have been recorded.
