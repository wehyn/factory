# Repository configuration

Agentic Factory reads optional repository settings from `.agentic-factory.json` at the
registered repository root. Keep this file under normal code review; builder changes to
policy, workflow, deployment, identity, permission, migration, privacy, or public API files
require human review.

## GitHub pull requests

GitHub observation uses the authenticated `gh` CLI and stores each observation in the local
ledger. The workspace can refresh a pull request by number. The merge gate requires an open,
non-draft pull request, a stable current head, every configured required check passing for that
head, and an independent approval submitted for that same head. A missing policy, check,
approval, or current-head match blocks the merge. Policy defaults keep automatic merging off.

Example:

```json
{
  "github": {
    "auto_merge_enabled": false,
    "required_checks": ["verify", "lint"],
    "auto_merge_paths": ["docs/", "*.md"],
    "max_auto_merge_changed_lines": 100
  }
}
```

Only small, low-risk changes within the path allowlist can reach the automatic merge decision.
Changes in security, authentication, permissions, privacy, migrations, deployment configuration,
secrets or credentials, API/schema/protocol surfaces, and GitHub workflows wait for Wayne's
review. Set `auto_merge_enabled` to `true` only when that behavior is intended for the repository.

Before a merge request, the gate rereads the pull request and its required checks and confirms
the head did not move during observation. The `gh pr merge` call also includes the exact head
SHA. Pull request create/merge action keys and receipts are durable; an interrupted action with
no receipt must be manually reconciled before retrying.

Pull request bodies are rendered from evidence stored in the local ledger: verification,
independent review, decisions, limitations, and worktree/commit provenance. If a category has no
recorded evidence, the body says so instead of implying it passed.

## Production watch

The app starts a read-only watch after it observes a merged pull request with a merge commit SHA.
Configure an HTTPS identity endpoint that returns the deployed commit SHA and an HTTPS smoke URL:

```json
{
  "production": {
    "environment_id": "production",
    "identity_url": "https://example.com/build-info.json",
    "identity_json_field": "deployment.commit_sha",
    "smoke_url": "https://example.com/health",
    "timeout_seconds": 10
  }
}
```

The environment ID is required. The default identity field is `commit_sha`; dotted paths such as
`deployment.commit_sha` are supported. URLs cannot include embedded credentials, and timeouts
must be between 1 and 60 seconds. The identity response must contain the full 40-character Git
commit SHA. A release is healthy only when that SHA matches the merged commit and the smoke
request returns HTTP 2xx. The merge gate will wait for review if the production watch
configuration is missing or invalid.

Missing identity or smoke evidence creates an unverified alert. A mismatched deployment waits;
a failing smoke check creates an alert. The app persists each observation and alert, sends a
desktop notification for a newly created alert, and keeps the alert visible in the workspace.
It does not roll back or automatically retry a failed observation. The watch probes each merged
SHA once; use **Run production check** to request another read-only observation. Alert
acknowledgements and resolved alerts remain in the local ledger across restarts and hidden windows.
Quitting the app records that monitoring stopped; the next launch marks merged runs unverified and
reports the monitoring gap until a fresh check completes. An unclean process exit is reported the
same way on the next launch.

Live GitHub behavior still requires a configured GitHub remote, an authenticated `gh`, and a
disposable repository for end-to-end acceptance. Core fixture tests do not prove those external
conditions.
