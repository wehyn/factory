use crate::{
    ledger::Ledger,
    mailbox::{Mailbox, McpPrincipal},
    model::{AgentId, AssignSliceRequest, MessageKind, MessageRecipient, RunId},
    repositories::RepositoryRegistry,
    scheduler::Scheduler,
    worktrees::WorktreeManager,
};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::{
    env,
    io::{BufRead, Write},
    path::PathBuf,
    sync::Arc,
};

const MAX_MCP_LINE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct FactoryMcpConfig {
    command: PathBuf,
    args: Vec<String>,
    ledger_path: PathBuf,
    worktree_root: PathBuf,
    principal: McpPrincipal,
}

impl FactoryMcpConfig {
    pub fn manager(command: PathBuf, ledger_path: PathBuf, worktree_root: PathBuf) -> Self {
        Self {
            command,
            args: Vec::new(),
            ledger_path,
            worktree_root,
            principal: McpPrincipal::Manager,
        }
    }

    pub fn agent(
        command: PathBuf,
        args: Vec<String>,
        ledger_path: PathBuf,
        worktree_root: PathBuf,
        run_id: RunId,
        agent_id: AgentId,
    ) -> Self {
        Self {
            command,
            args,
            ledger_path,
            worktree_root,
            principal: McpPrincipal::Agent { run_id, agent_id },
        }
    }

    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    /// Codex CLI `--config` key/value overlays. These values are process arguments;
    /// this helper never writes the user's global Codex configuration.
    pub fn config_overrides(&self) -> Result<Vec<String>> {
        let command = self
            .command
            .to_str()
            .ok_or_else(|| anyhow!("factory MCP executable path must be valid UTF-8"))?;
        let ledger = self
            .ledger_path
            .to_str()
            .ok_or_else(|| anyhow!("factory ledger path must be valid UTF-8"))?;
        let worktrees = self
            .worktree_root
            .to_str()
            .ok_or_else(|| anyhow!("factory worktree path must be valid UTF-8"))?;
        let principal = match self.principal {
            McpPrincipal::Manager => "manager".to_owned(),
            McpPrincipal::Agent { run_id, agent_id } => {
                format!("agent:{run_id}:{agent_id}")
            }
        };
        let overrides = vec![
            format!("mcp_servers.factory.command={}", toml_string(command)?),
            format!(
                "mcp_servers.factory.args={}",
                serde_json::to_string(&self.args)?
            ),
            "mcp_servers.factory.enabled=true".to_owned(),
            "mcp_servers.factory.required=true".to_owned(),
            format!(
                "mcp_servers.factory.env.FACTORY_LEDGER_PATH={}",
                toml_string(ledger)?
            ),
            format!(
                "mcp_servers.factory.env.FACTORY_WORKTREE_ROOT={}",
                toml_string(worktrees)?
            ),
            format!(
                "mcp_servers.factory.env.FACTORY_PRINCIPAL={}",
                toml_string(&principal)?
            ),
        ];
        Ok(overrides)
    }
}

fn toml_string(value: &str) -> Result<String> {
    // JSON basic strings use the same escaping needed for TOML basic strings here.
    Ok(serde_json::to_string(value)?)
}

#[derive(Clone)]
pub struct FactoryMcpServer {
    mailbox: Mailbox,
    registry: RepositoryRegistry,
    principal: McpPrincipal,
    scheduler: Option<Scheduler>,
}

#[derive(Clone, Copy)]
struct ToolSpec {
    name: &'static str,
    description: &'static str,
    schema: &'static str,
    read_only: bool,
    idempotent: bool,
}

const MANAGER_TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "factory_list_repositories",
        description: "List repositories registered with Agentic Factory.",
        schema: r#"{"type":"object","properties":{},"additionalProperties":false}"#,
        read_only: true,
        idempotent: true,
    },
    ToolSpec {
        name: "factory_assign_slice",
        description: "Assign a bounded slice to an isolated worker worktree. Manager only.",
        schema: r#"{"type":"object","properties":{"run_id":{"type":"string"},"assignment_key":{"type":"string"},"objective":{"type":"string"},"allowed_paths":{"type":"array","items":{"type":"string"}},"dependency_ids":{"type":"array","items":{"type":"string"}},"contract_keys":{"type":"array","items":{"type":"string"}},"acceptance_evidence":{"type":"string"}},"required":["run_id","assignment_key","objective","allowed_paths","dependency_ids","contract_keys","acceptance_evidence"],"additionalProperties":false}"#,
        read_only: false,
        idempotent: true,
    },
    ToolSpec {
        name: "factory_list_messages",
        description: "Read the durable manager ledger for one active run.",
        schema: r#"{"type":"object","properties":{"run_id":{"type":"string"}},"required":["run_id"],"additionalProperties":false}"#,
        read_only: true,
        idempotent: true,
    },
    ToolSpec {
        name: "factory_resolve_contract",
        description: "Resolve a proposed or conflicting run contract. Manager only.",
        schema: r#"{"type":"object","properties":{"run_id":{"type":"string"},"key":{"type":"string"},"body":{"type":"string"}},"required":["run_id","key","body"],"additionalProperties":false}"#,
        read_only: false,
        idempotent: false,
    },
    ToolSpec {
        name: "factory_send_message",
        description: "Send a directed message from the manager to a worker.",
        schema: r#"{"type":"object","properties":{"run_id":{"type":"string"},"to_agent_id":{"type":"string"},"kind":{"type":"string","enum":["question","answer","handoff","blocker","completion"]},"body":{"type":"string"}},"required":["run_id","to_agent_id","kind","body"],"additionalProperties":false}"#,
        read_only: false,
        idempotent: false,
    },
    ToolSpec {
        name: "factory_list_assignments",
        description: "List bounded assignments and their scheduler state for one active run.",
        schema: r#"{"type":"object","properties":{"run_id":{"type":"string"}},"required":["run_id"],"additionalProperties":false}"#,
        read_only: true,
        idempotent: true,
    },
    ToolSpec {
        name: "factory_list_blockers",
        description: "List unresolved worker, dependency, or integration blockers for a run.",
        schema: r#"{"type":"object","properties":{"run_id":{"type":"string"}},"required":["run_id"],"additionalProperties":false}"#,
        read_only: true,
        idempotent: true,
    },
    ToolSpec {
        name: "factory_complete_slice",
        description:
            "Record completion evidence for a worker and validate its commit and file scope.",
        schema: r#"{"type":"object","properties":{"agent_id":{"type":"string"},"evidence":{"type":"string"}},"required":["agent_id","evidence"],"additionalProperties":false}"#,
        read_only: false,
        idempotent: true,
    },
    ToolSpec {
        name: "factory_integrate_completed",
        description:
            "Integrate completed dependency-ready worker commits into the manager worktree.",
        schema: r#"{"type":"object","properties":{"run_id":{"type":"string"}},"required":["run_id"],"additionalProperties":false}"#,
        read_only: false,
        idempotent: true,
    },
    ToolSpec {
        name: "factory_resolve_blocker",
        description: "Acknowledge a manager blocker after recovery or a deliberate decision.",
        schema: r#"{"type":"object","properties":{"blocker_id":{"type":"string"}},"required":["blocker_id"],"additionalProperties":false}"#,
        read_only: false,
        idempotent: true,
    },
    ToolSpec {
        name: "factory_retry_slice",
        description: "Queue an eligible bounded worker retry after its previous process and worktree are accounted for.",
        schema: r#"{"type":"object","properties":{"slice_id":{"type":"string"}},"required":["slice_id"],"additionalProperties":false}"#,
        read_only: false,
        idempotent: true,
    },
];

const AGENT_TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "factory_check_inbox",
        description: "Read unacknowledged messages directed to this worker.",
        schema: r#"{"type":"object","properties":{},"additionalProperties":false}"#,
        read_only: true,
        idempotent: true,
    },
    ToolSpec {
        name: "factory_get_contracts",
        description:
            "Read authoritative contract decisions attached to this worker's assigned slice.",
        schema: r#"{"type":"object","properties":{},"additionalProperties":false}"#,
        read_only: true,
        idempotent: true,
    },
    ToolSpec {
        name: "factory_acknowledge_message",
        description: "Acknowledge a message directed to this worker.",
        schema: r#"{"type":"object","properties":{"message_id":{"type":"string"}},"required":["message_id"],"additionalProperties":false}"#,
        read_only: false,
        idempotent: true,
    },
    ToolSpec {
        name: "factory_send_message",
        description: "Send a message from this worker to the manager or another worker.",
        schema: r#"{"type":"object","properties":{"to_agent_id":{"type":"string"},"kind":{"type":"string","enum":["question","answer","handoff","contract","blocker","completion"]},"body":{"type":"string"},"contract_key":{"type":"string"}},"required":["kind","body"],"additionalProperties":false}"#,
        read_only: false,
        idempotent: false,
    },
];

impl FactoryMcpServer {
    pub fn new(mailbox: Mailbox, registry: RepositoryRegistry, principal: McpPrincipal) -> Self {
        Self {
            mailbox,
            registry,
            principal,
            scheduler: None,
        }
    }

    pub fn with_scheduler(mut self, scheduler: Scheduler) -> Self {
        self.scheduler = Some(scheduler);
        self
    }

    pub fn list_tools(&self) -> Vec<Value> {
        self.tools()
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": serde_json::from_str::<Value>(tool.schema).expect("static MCP schema"),
                    "annotations": {
                        "readOnlyHint": tool.read_only,
                        "destructiveHint": false,
                        "idempotentHint": tool.idempotent,
                        "openWorldHint": false
                    }
                })
            })
            .collect()
    }

    fn tools(&self) -> &'static [ToolSpec] {
        match self.principal {
            McpPrincipal::Manager => MANAGER_TOOLS,
            McpPrincipal::Agent { .. } => AGENT_TOOLS,
        }
    }

    pub fn handle_json_rpc(&self, request: &Value) -> Result<Value> {
        let Some(object) = request.as_object() else {
            return Ok(rpc_error(Value::Null, -32600, "Invalid Request"));
        };
        let id = object.get("id").cloned();
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Ok(rpc_error(
                id.unwrap_or(Value::Null),
                -32600,
                "Invalid Request",
            ));
        }
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            return Ok(rpc_error(
                id.unwrap_or(Value::Null),
                -32600,
                "Invalid Request",
            ));
        };
        let id = id.unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": "2025-03-26",
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "agentic-factory", "version": env!("CARGO_PKG_VERSION")}
            })),
            "notifications/initialized" | "notifications/cancelled" => Ok(Value::Null),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": self.list_tools() })),
            "tools/call" => self.call_tool(object.get("params").unwrap_or(&Value::Null)),
            _ => return Ok(rpc_error(id, -32601, "Method not found")),
        };
        if id.is_null() {
            return Ok(Value::Null);
        }
        Ok(match result {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err(error) => {
                let safe = crate::RedactedOutput::new(error.to_string());
                rpc_error(id, -32603, safe.as_str())
            }
        })
    }

    fn call_tool(&self, params: &Value) -> Result<Value> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("tool call is missing a name"))?;
        if !self.tools().iter().any(|tool| tool.name == name) {
            return Ok(tool_error(format!(
                "tool '{name}' is not available to this principal"
            )));
        }
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !arguments.is_object() {
            return Ok(tool_error("tool arguments must be an object"));
        }
        let result = self.dispatch(name, &arguments);
        match result {
            Ok(value) => {
                self.mailbox.record_tool_invocation(name, self.principal)?;
                Ok(json!({
                    "content": [{"type":"text","text":serde_json::to_string(&value)?}],
                    "structuredContent": value,
                    "isError": false
                }))
            }
            Err(error) => {
                let safe = crate::RedactedOutput::new(error.to_string());
                Ok(tool_error(safe.as_str()))
            }
        }
    }

    fn dispatch(&self, name: &str, arguments: &Value) -> Result<Value> {
        match (self.principal, name) {
            (McpPrincipal::Manager, "factory_list_repositories") => {
                Ok(serde_json::to_value(self.registry.list()?)?)
            }
            (McpPrincipal::Manager, "factory_assign_slice") => {
                let request = AssignSliceRequest {
                    run_id: parse_id(required_str(arguments, "run_id")?)?,
                    assignment_key: required_str(arguments, "assignment_key")?.to_owned(),
                    objective: required_str(arguments, "objective")?.to_owned(),
                    allowed_paths: required_string_array(arguments, "allowed_paths")?,
                    dependency_ids: required_string_array(arguments, "dependency_ids")?
                        .iter()
                        .map(|value| parse_id(value))
                        .collect::<Result<_>>()?,
                    contract_keys: required_string_array(arguments, "contract_keys")?,
                    acceptance_evidence: required_str(arguments, "acceptance_evidence")?.to_owned(),
                };
                Ok(serde_json::to_value(
                    self.mailbox.assign_slice(McpPrincipal::Manager, request)?,
                )?)
            }
            (McpPrincipal::Manager, "factory_list_messages") => {
                let run_id = parse_id(required_str(arguments, "run_id")?)?;
                Ok(serde_json::to_value(self.mailbox.list_messages(run_id)?)?)
            }
            (McpPrincipal::Manager, "factory_list_assignments") => {
                let run_id = parse_id(required_str(arguments, "run_id")?)?;
                Ok(serde_json::to_value(
                    self.mailbox.list_assignments(run_id)?,
                )?)
            }
            (McpPrincipal::Manager, "factory_list_blockers") => {
                let run_id = parse_id(required_str(arguments, "run_id")?)?;
                Ok(serde_json::to_value(
                    self.manager_scheduler()?.list_blockers(run_id)?,
                )?)
            }
            (McpPrincipal::Manager, "factory_complete_slice") => {
                let agent_id = parse_id(required_str(arguments, "agent_id")?)?;
                Ok(serde_json::to_value(
                    self.manager_scheduler()?
                        .complete_slice(agent_id, required_str(arguments, "evidence")?)?,
                )?)
            }
            (McpPrincipal::Manager, "factory_integrate_completed") => {
                let run_id = parse_id(required_str(arguments, "run_id")?)?;
                Ok(serde_json::to_value(
                    self.manager_scheduler()?.integrate_completed(run_id)?,
                )?)
            }
            (McpPrincipal::Manager, "factory_resolve_blocker") => {
                let blocker_id = parse_id(required_str(arguments, "blocker_id")?)?;
                self.manager_scheduler()?.resolve_blocker(blocker_id)?;
                Ok(json!({"resolved": blocker_id}))
            }
            (McpPrincipal::Manager, "factory_retry_slice") => {
                let slice_id = parse_id(required_str(arguments, "slice_id")?)?;
                self.manager_scheduler()?.retry_slice(slice_id)?;
                Ok(json!({"queued": slice_id}))
            }
            (McpPrincipal::Manager, "factory_resolve_contract") => {
                let run_id = parse_id(required_str(arguments, "run_id")?)?;
                Ok(serde_json::to_value(self.mailbox.resolve_contract(
                    McpPrincipal::Manager,
                    run_id,
                    required_str(arguments, "key")?,
                    required_str(arguments, "body")?,
                )?)?)
            }
            (McpPrincipal::Manager, "factory_send_message") => {
                let run_id = parse_id(required_str(arguments, "run_id")?)?;
                let to_agent = parse_id(required_str(arguments, "to_agent_id")?)?;
                let kind = parse_message_kind(required_str(arguments, "kind")?)?;
                Ok(serde_json::to_value(self.mailbox.send_message(
                    McpPrincipal::Manager,
                    run_id,
                    MessageRecipient::Agent(to_agent),
                    kind,
                    required_str(arguments, "body")?,
                    optional_str(arguments, "contract_key"),
                )?)?)
            }
            (McpPrincipal::Agent { run_id, agent_id }, "factory_check_inbox") => Ok(
                serde_json::to_value(self.mailbox.deliver_pending(run_id, agent_id)?)?,
            ),
            (McpPrincipal::Agent { run_id, agent_id }, "factory_get_contracts") => Ok(
                serde_json::to_value(self.mailbox.contracts_for_agent(run_id, agent_id)?)?,
            ),
            (McpPrincipal::Agent { run_id, agent_id }, "factory_acknowledge_message") => {
                let message_id = parse_id(required_str(arguments, "message_id")?)?;
                self.mailbox.acknowledge(run_id, agent_id, message_id)?;
                Ok(json!({"acknowledged": message_id}))
            }
            (McpPrincipal::Agent { run_id, .. }, "factory_send_message") => {
                let recipient = match optional_str(arguments, "to_agent_id") {
                    Some(raw) => MessageRecipient::Agent(parse_id(raw)?),
                    None => MessageRecipient::Manager,
                };
                Ok(serde_json::to_value(self.mailbox.send_message(
                    self.principal,
                    run_id,
                    recipient,
                    parse_message_kind(required_str(arguments, "kind")?)?,
                    required_str(arguments, "body")?,
                    optional_str(arguments, "contract_key"),
                )?)?)
            }
            _ => bail!("tool '{name}' is not authorized for this principal"),
        }
    }

    fn manager_scheduler(&self) -> Result<&Scheduler> {
        if self.principal != McpPrincipal::Manager {
            bail!("only the manager can access scheduler controls");
        }
        self.scheduler
            .as_ref()
            .ok_or_else(|| anyhow!("scheduler control is unavailable in this MCP process"))
    }

    pub fn serve_stdio<R: BufRead, W: Write>(&self, reader: &mut R, writer: &mut W) -> Result<()> {
        let mut line = Vec::new();
        loop {
            line.clear();
            let (bytes, overflow) = read_bounded_line(reader, &mut line)?;
            if bytes == 0 {
                return Ok(());
            }
            if overflow {
                let response =
                    rpc_error(Value::Null, -32700, "Request line exceeded the size limit");
                serde_json::to_writer(&mut *writer, &response)?;
                writer.write_all(b"\n")?;
                continue;
            }
            let parsed = serde_json::from_slice::<Value>(&line);
            let response = match parsed {
                Ok(request) => {
                    let notification = request.get("id").is_none();
                    let response = self.handle_json_rpc(&request)?;
                    if notification || response.is_null() {
                        continue;
                    }
                    response
                }
                Err(_) => rpc_error(Value::Null, -32700, "Parse error"),
            };
            serde_json::to_writer(&mut *writer, &response)?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
    }
}

fn read_bounded_line<R: BufRead>(reader: &mut R, line: &mut Vec<u8>) -> Result<(usize, bool)> {
    let mut total = 0usize;
    let mut overflow = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok((total, overflow));
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        let remaining = MAX_MCP_LINE_BYTES
            .saturating_add(1)
            .saturating_sub(line.len());
        line.extend_from_slice(&available[..consumed.min(remaining)]);
        total = total.saturating_add(consumed);
        if line.len() > MAX_MCP_LINE_BYTES {
            overflow = true;
            line.clear();
        }
        reader.consume(consumed);
        if newline.is_some() {
            return Ok((total, overflow));
        }
    }
}

fn tool_error(message: impl Into<String>) -> Value {
    json!({
        "content": [{"type":"text","text":message.into()}],
        "isError": true
    })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

fn required_str<'a>(object: &'a Value, key: &str) -> Result<&'a str> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("tool argument '{key}' must be a string"))
}

fn optional_str<'a>(object: &'a Value, key: &str) -> Option<&'a str> {
    object.get(key).and_then(Value::as_str)
}

fn required_string_array(object: &Value, key: &str) -> Result<Vec<String>> {
    object
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("tool argument '{key}' must be an array of strings"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("tool argument '{key}' must contain only strings"))
        })
        .collect()
}

fn parse_id<T: From<uuid::Uuid>>(value: &str) -> Result<T> {
    Ok(uuid::Uuid::parse_str(value)
        .with_context(|| format!("invalid UUID value for MCP tool"))?
        .into())
}

fn parse_message_kind(value: &str) -> Result<MessageKind> {
    match value {
        "question" => Ok(MessageKind::Question),
        "answer" => Ok(MessageKind::Answer),
        "handoff" => Ok(MessageKind::Handoff),
        "contract" => Ok(MessageKind::Contract),
        "blocker" => Ok(MessageKind::Blocker),
        "completion" => Ok(MessageKind::Completion),
        _ => bail!("message kind is not supported"),
    }
}

pub fn serve_stdio_from_env() -> Result<()> {
    let ledger_path = required_env_path("FACTORY_LEDGER_PATH")?;
    let worktree_root = required_env_path("FACTORY_WORKTREE_ROOT")?;
    let principal = match env::var("FACTORY_PRINCIPAL").as_deref() {
        Ok("manager") => McpPrincipal::Manager,
        Ok(value) if value.starts_with("agent:") => {
            let mut parts = value[6..].split(':');
            let run_id = parse_id(parts.next().unwrap_or_default())?;
            let agent_id = parse_id(parts.next().unwrap_or_default())?;
            if parts.next().is_some() {
                bail!("invalid factory MCP agent identity");
            }
            McpPrincipal::Agent { run_id, agent_id }
        }
        _ => bail!("factory MCP principal is missing or invalid"),
    };
    let ledger = Ledger::open(&ledger_path)?;
    let worktrees = WorktreeManager::new(ledger.clone(), worktree_root)?;
    let mailbox = Mailbox::new(ledger.clone(), worktrees.clone());
    let registry = RepositoryRegistry::new(ledger.clone());
    let server = FactoryMcpServer::new(mailbox, registry, principal);
    let server = if principal == McpPrincipal::Manager {
        server.with_scheduler(Scheduler::control(Arc::new(ledger), worktrees))
    } else {
        server
    };
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = std::io::BufReader::new(stdin.lock());
    let mut writer = std::io::BufWriter::new(stdout.lock());
    server.serve_stdio(&mut reader, &mut writer)
}

fn required_env_path(name: &str) -> Result<PathBuf> {
    let value =
        env::var_os(name).ok_or_else(|| anyhow!("required factory MCP setting is missing"))?;
    let path = PathBuf::from(value);
    if path.as_os_str().is_empty() {
        bail!("required factory MCP setting is empty");
    }
    Ok(path)
}
