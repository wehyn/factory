# Codex App Server protocol probe

## Environment and safety boundary

- Codex CLI: `codex-cli 0.157.0` (`codex --version`, exit status 0).
- Server: `codex app-server --stdio`.
- Schema command: `codex app-server generate-json-schema --out work/research-app-server-schema` (exit status 0).
- Probe command: `python3 work/research-live-probe/probe.py` (exit status 0).
- The probe created a fresh temporary directory, ran `git init -q` there, and removed it after the run. It did not open a user repository.
- The thread was created with `sandbox: "read-only"`, `approvalPolicy: "never"`, and `ephemeral: true`.
- The only prompt was: “Reply with exactly the word hello. Do not use tools or modify files.” The App Server stderr was discarded; no auth files or process environment were inspected or saved.

## Confirmed request contract

1. Send JSON-RPC `initialize` with `clientInfo: { name, version }` and `capabilities.experimentalApi: false`.
2. Send the `initialized` notification.
3. Send `thread/start` with the canonical disposable repository `cwd`, `sandbox: "read-only"`, `approvalPolicy: "never"`, and `ephemeral: true`. The response contains `thread.id`.
4. Send `turn/start` with `threadId` and `input: [{ type: "text", text: <prompt> }]`. The response contains `turn.id`.
5. Interrupt with `turn/interrupt` and both `threadId` and `turnId`.

## Observed harmless notification sequence

```text
thread/started
turn/started                 status=inProgress
item/started                 type=userMessage
item/completed               type=userMessage
item/started                 type=agentMessage
item/completed               type=agentMessage, text="hello"
turn/completed               status=completed
```

Other App Server notifications were also seen (`item/agentMessage/delta`, token usage, account status, and MCP startup status). The adapter ignores them for display and completion decisions. It persists only completed `agentMessage` text, after redaction.

## Adapter verification

- `cargo test -p factory-core --test codex_protocol`: fake App Server used the observed event names and payload shapes, fragmented a JSON-RPC response across writes, and passed output preservation, turn completion, interruption, and exactly-once failure assertions.
- `cargo test -p factory-core --test codex_protocol live_codex_app_server_persists_a_harmless_disposable_turn -- --ignored --exact --nocapture`: passed twice against the installed CLI, including after process-tree shutdown was fixed. The latest run completed in 10.74 seconds, stored the real `agentMessage` text `hello`, and observed `turn/completed` status `completed`; shutdown left no `codex app-server --stdio` process running.
- The live adapter check creates and removes a fresh temporary Git repository, passes the read-only/never-approval/ephemeral settings above, and asks only for the harmless one-word response.
