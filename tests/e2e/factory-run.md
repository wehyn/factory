# Agentic Factory acceptance run

This checklist separates repeatable fixture proof from live-service proof. Do not mark a live
step complete based on a fake CLI, unit test, or a code review.

## Evidence record

- Date and operator: 2026-09-29 / Codex local run
- App build / commit: `Agentic Factory Preview` debug bundle (`dev.wayne.agenticfactory.preview`), rebuilt from code commit `3921381` on top of `origin/main` commit `7e5fbcb`; includes URL-bound create recovery and server-owned provenance input
- macOS version: 27.0
- Codex CLI version: 0.157.0
- GitHub CLI version and authentication status (never record credentials): 2.101.0; authenticated to github.com (credentials omitted)
- Disposable repository A and setup commit: private [`wehyn/factory-e2e-a`](https://github.com/wehyn/factory-e2e-a), `main` at `688e778c1ab7cc3c3aa0bf9935b044b51c07c290`; initial README commit `141d26124b855ed08e577248bb946940765f8d3d`, setup commit `b46881653daa357a558700d3a24ef4d31d3a6164`
- Disposable repository B and setup commit: private [`wehyn/factory-e2e-b`](https://github.com/wehyn/factory-e2e-b), `main` at `321da94467096a815065b4d3d376becdc4259173`; initial README commit `ae01287a43060f71acc88612a984f3dab50f50f9`, setup commit `7307d1e053fe4292d82ac7931ffb88d06ebf65e9`
- GitHub account: `wehyn`; current account has no independent collaborator on either disposable repo
- Isolated preview app PID: `46647`; bundle ID `dev.wayne.agenticfactory.preview`
- Production test environment ID: not configured
- Screenshots or log paths: `/tmp/agentic-factory-preview-final.png`; bundle at `target/debug/bundle/macos/Agentic Factory Preview.app`

## Prepare disposable repositories

- [x] Create two private disposable repositories with distinct GitHub remotes and committed
  `main` branches. Do not point the app at a user project.
- [x] Register both repositories in the isolated preview app and confirm their canonical roots.
- [ ] Add a production policy to each repo with distinct `production.environment_id` values,
  HTTPS deployment identity URLs, a smoke URL, and a short timeout.
- [x] Configure the app's `verify` required check and docs-only auto-merge allowlist in both
  repositories. Automatic merging remains disabled until live review and production evidence are
  available. GitHub branch-protection API returned 403 because this account plan does not enable
  branch protection on private repositories. Both workflow checkouts use `fetch-depth: 0` so the
  base commit is available to `git diff --check`.
- [x] Start the isolated preview app (PID `11107`) and create one run per repository. Run A is
  `4ed97579-9a7d-4868-86af-20130e8a0ece`, integration branch
  `factory/run-4ed97579-9a7d-4868-86af-20130e8a0ece`; run B is
  `1be942e4-2d2b-4704-a764-1f0d879f154f`, integration branch
  `factory/run-1be942e4-2d2b-4704-a764-1f0d879f154f`. The two runs are linked in the ledger.

## Concurrent linked runs and worker isolation

- [x] Create one run in each repository and link the two runs from the workspace.
- [x] Split work into non-overlapping docs-only changes in the two repositories. The UI showed
  separate integration worktrees and exclusive builder worktrees.
- [x] Start one builder per run. Both outputs, branches, and directed handoffs appeared on the
  canvas. Run B completed and integrated; run A's manager completion call was rejected and the
  dirty worker worktree was preserved as a recovery blocker.
- [x] Record run IDs, integration branches, source/integration commits, worker IDs, and worktree
  paths below. The packaged preview restart restored the same runs and messages without launching
  duplicate workers.

Run A: `4ed97579-9a7d-4868-86af-20130e8a0ece`; worker
`e0bb3f49-28b5-4c20-95e6-beb0e29bde68`; assigned slice
`8dbad657-58e8-4edd-adb5-f43376876bf8`; integration branch
`factory/run-4ed97579-9a7d-4868-86af-20130e8a0ece`. Its worker created only
`docs/acceptance-a.md` with `FACTORY_E2E_FAIL`; Factory did not record a source commit and now
reports the dirty worker tree as needing manager recovery.

Run B: `1be942e4-2d2b-4704-a764-1f0d879f154f`; worker
`7ebe9709-2454-40f4-a6c5-ea07f85bc11c`; source commit
`2febb2a61edc12aa4f3093f7eb57f8c2160d5e7f`; integration branch
`factory/run-1be942e4-2d2b-4704-a764-1f0d879f154f`; integration commit
`ebf6334db7e23777ea49af5f2876fdfefb19f7b1`. Its only file change is `docs/acceptance-b.md`.

## GitHub gate and durable action receipts

Run these cases on disposable pull requests; record the PR number and head SHA for every case.

- [x] Refresh a PR while the `verify` check is failed. The app blocks merge; after the workflow
  checkout fix, the same PR's check passes and the gate advances to the independent-review
  requirement.
- [ ] Push a new commit after checks pass. Refresh the PR; stale checks and an approval from the
  previous head cannot authorize a merge.
- Live partial evidence: after the original `verify` passed, a docs-only head-revalidation probe
  was pushed to run B as `7bec48bc514f01ae37578f6602fdabc2084b664f`. `verify` passed for that SHA
  in run `36566050494`, and the preview app refreshed PR #1 against the new head. No approval
  existed on the previous head, so approval invalidation remains unverified and this item stays
  unchecked.
- [ ] Change a permission, migration, deployment/workflow, secret, privacy, security, or public API
  surface. The gate waits for Wayne even when checks pass.
- [ ] Make a small docs-only change with every configured check passing and an independent review
  approval on the current head. Confirm the UI shows the eligible merge action.
- [ ] Click **Merge with current checks** once. Confirm the merge command includes the recorded
  head SHA, then refresh and verify the merged commit SHA.
- [ ] Repeat the same create/merge action key through a fixture or controlled retry. Confirm the
  ledger returns the original receipt and GitHub receives no duplicate action.
- [x] Create a pull request from the integrated run. The app published only run B's integration
  branch and the PR body included verification, review, decision, limitation, and commit/worktree
  provenance. The app recorded PR #1 at head
  `ebf6334db7e23777ea49af5f2876fdfefb19f7b1`; `verify` passed in run
  `36559445822`. The PR remains open because no independent current-head approval is available.
- [x] Recover the interrupted PR-creation receipt: after a successful live refresh of PR #1, the
  ledger action `create-pr:1be942e4-2d2b-4704-a764-1f0d879f154f` is `completed` with a stored
  result. A regression test confirms retrying that key does not create a second PR.
- Current PR #1 head after the follow-up probe is `7bec48bc514f01ae37578f6602fdabc2084b664f`;
  GitHub reports its `verify` check passed. The PR remains open and the preview gate waits for an
  independent current-head approval.
- [x] Fixture-only recovery and provenance regressions: an observed PR completes a pending create
  only when its URL matches the URL returned by that create; an older matching PR cannot complete
  a create command that failed before returning a URL. A forged `worktree_commits` field in the UI
  payload is ignored, while server-generated integration provenance remains in the PR body. These
  tests do not replace the unchecked live retry and merge cases above.

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
- [x] Fixture-only: a different deployment SHA stays `waiting_for_deployment`; its alert is
  persisted and deduplicated across ledger reopen. Sensitive-path fixtures also keep security,
  permission, migration, deployment, secret, privacy, and public-interface edits in human review.
  These fixture checks do not replace the unchecked live deployment observations above.

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
- [ ] Record live deployment observations; none were available because production identity and
  smoke endpoints are not configured. PR head/check evidence is recorded above.
- [x] List skipped steps and why. This product is not production-ready until the real Codex,
  GitHub, production watch, and process-lifecycle evidence above has been recorded.

## Current checkout evidence

This implementation run verifies the merge path against a fake `gh` executable, production
outcomes against a fake `curl` executable, durable SQLite recovery, workspace builds, and a
packaged Tauri build. Fresh verification after the R8/R9 fixture additions passed `cargo fmt --all
-- --check`, `cargo test --workspace` (75 passed, 3 ignored), `npm test -- --run` (4 passed),
`npm run build`, and `git diff --check`; the isolated preview app rebuilt successfully from code
commit `3921381`. The recorded 2160×1440 visual review and the three individually run App
Server/MCP/parallel-worker integration cases are from the earlier acceptance run.

Skipped live steps and why:

- Independent current-head approval and a successful merge: only the `wehyn` account is present on
  both private disposable repos, so this account cannot supply the independent review the gate
  requires. The gate will not be bypassed to manufacture a merge receipt.
- Run A source-commit recording and integration: the manager completion operation rejected the
  completed worker's assignment, and restart recovery preserved its dirty worktree and marked the
  run as needing manager recovery.
- Production identity, matching merge SHA, failing smoke check, and live alert recovery: no
  production test environment or identity/smoke endpoints are configured.
- Active-run close/reopen/restart against the final packaged bundle with production monitoring:
  local lifecycle primitives have prior evidence, but this combined release acceptance has not
  been run.

The live GitHub, production-watch, and combined process-lifecycle boxes above remain unverified;
the product is not production-ready until those steps have been recorded.
