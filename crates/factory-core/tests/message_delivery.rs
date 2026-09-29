use factory_core::{
    AgentId, AssignSliceRequest, CodexRunner, ContractStatus, Event, EventKind, FactoryMcpConfig,
    FactoryMcpServer, Ledger, Mailbox, McpPrincipal, MessageKind, MessageRecipient,
    RepositoryRegistry, RunId, SessionId, SliceStatus, WorktreeManager,
};
use serde_json::{json, Value};
use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};
use tempfile::TempDir;
use uuid::Uuid;

struct Fixture {
    _temp: TempDir,
    base_sha: String,
    registry: RepositoryRegistry,
    worktrees: WorktreeManager,
    mailbox: Mailbox,
    run_id: RunId,
}

impl Fixture {
    fn new() -> anyhow::Result<Self> {
        let temp = tempfile::tempdir()?;
        let repository = temp.path().join("repository");
        fs::create_dir_all(&repository)?;
        git(&repository, &["init", "--initial-branch=main"])?;
        git(&repository, &["config", "user.name", "Factory Test"])?;
        git(
            &repository,
            &["config", "user.email", "factory-test@example.invalid"],
        )?;
        fs::create_dir_all(repository.join("src"))?;
        fs::write(repository.join("README.md"), "base\n")?;
        fs::write(repository.join("src/api.rs"), "pub fn api() {}\n")?;
        fs::write(repository.join("src/ui.rs"), "pub fn ui() {}\n")?;
        git(&repository, &["add", "."])?;
        git(&repository, &["commit", "-m", "initial"])?;
        let base_sha = git(&repository, &["rev-parse", "HEAD"])?;

        let ledger = Ledger::open(&temp.path().join("factory.sqlite"))?;
        let registry = RepositoryRegistry::new(ledger.clone());
        let worktrees = WorktreeManager::new(ledger.clone(), temp.path().join("worktrees"))?;
        let mailbox = Mailbox::new(ledger.clone(), worktrees.clone());
        let registered = registry.register(&repository)?;
        let run_id = RunId(Uuid::new_v4());
        worktrees.create_run_worktree(registered.id, run_id, &base_sha)?;

        Ok(Self {
            _temp: temp,
            base_sha,
            registry,
            worktrees,
            mailbox,
            run_id,
        })
    }

    fn agent(&self) -> AgentId {
        AgentId(Uuid::new_v4())
    }

    fn add_agent(&self, agent_id: AgentId) -> anyhow::Result<()> {
        self.worktrees
            .create_agent_worktree(self.run_id, agent_id, &self.base_sha)?;
        Ok(())
    }
}

fn git(root: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn assignment(
    run_id: RunId,
    assignment_key: &str,
    allowed_paths: &[&str],
    dependencies: Vec<factory_core::SliceId>,
    contract_keys: &[&str],
) -> AssignSliceRequest {
    AssignSliceRequest {
        run_id,
        assignment_key: assignment_key.to_owned(),
        objective: format!("Implement {assignment_key}"),
        allowed_paths: allowed_paths
            .iter()
            .map(|path| (*path).to_owned())
            .collect(),
        dependency_ids: dependencies,
        contract_keys: contract_keys.iter().map(|key| (*key).to_owned()).collect(),
        acceptance_evidence: format!("Verify {assignment_key}"),
    }
}

#[test]
fn directed_messages_are_single_inbox_records_and_visible_to_the_manager() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let agent_a = fixture.agent();
    let agent_b = fixture.agent();
    let agent_c = fixture.agent();
    for agent in [agent_a, agent_b, agent_c] {
        fixture.add_agent(agent)?;
    }

    let message = fixture.mailbox.send_message(
        McpPrincipal::Agent {
            run_id: fixture.run_id,
            agent_id: agent_a,
        },
        fixture.run_id,
        MessageRecipient::Agent(agent_b),
        MessageKind::Question,
        "Please confirm the response type before editing.",
        None,
    )?;

    let inbox = fixture.mailbox.deliver_pending(fixture.run_id, agent_b)?;
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].id, message.id);
    assert_eq!(inbox[0].from, MessageRecipient::Agent(agent_a));
    assert_eq!(
        inbox[0].body,
        "Please confirm the response type before editing."
    );

    let manager_ledger = fixture.mailbox.list_messages(fixture.run_id)?;
    assert_eq!(manager_ledger.len(), 1);
    assert_eq!(manager_ledger[0].id, message.id);
    assert!(fixture
        .mailbox
        .deliver_pending(fixture.run_id, agent_c)?
        .is_empty());

    fixture
        .mailbox
        .acknowledge(fixture.run_id, agent_b, message.id)?;
    assert!(fixture
        .mailbox
        .deliver_pending(fixture.run_id, agent_b)?
        .is_empty());
    assert_eq!(fixture.mailbox.list_messages(fixture.run_id)?.len(), 1);
    Ok(())
}

#[test]
fn manager_assignments_validate_dependencies_scope_and_are_idempotent() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let manager = McpPrincipal::Manager;

    let api = fixture.mailbox.assign_slice(
        manager,
        assignment(fixture.run_id, "api-contract", &["src/api.rs"], vec![], &[]),
    )?;
    assert_eq!(api.status, SliceStatus::Queued);
    assert_eq!(
        fixture
            .worktrees
            .get_worktree(api.worktree_id.expect("assignment worktree"))?
            .base_sha,
        fixture.base_sha
    );

    let retry = fixture.mailbox.assign_slice(
        manager,
        assignment(fixture.run_id, "api-contract", &["src/api.rs"], vec![], &[]),
    )?;
    assert_eq!(retry.id, api.id);
    assert_eq!(fixture.mailbox.list_assignments(fixture.run_id)?.len(), 1);

    let dependent = fixture.mailbox.assign_slice(
        manager,
        assignment(
            fixture.run_id,
            "ui-consumer",
            &["src/ui.rs"],
            vec![api.id],
            &[],
        ),
    )?;
    assert_eq!(dependent.dependency_ids, vec![api.id]);

    let invalid_dependency = fixture.mailbox.assign_slice(
        manager,
        assignment(
            fixture.run_id,
            "unknown-dependency",
            &["src/other.rs"],
            vec![factory_core::SliceId::new()],
            &[],
        ),
    );
    assert!(invalid_dependency.is_err());

    let cross_scope = fixture.mailbox.assign_slice(
        manager,
        assignment(fixture.run_id, "overlapping-scope", &["src"], vec![], &[]),
    );
    assert!(cross_scope.is_err());
    assert_eq!(fixture.mailbox.list_assignments(fixture.run_id)?.len(), 2);
    Ok(())
}

#[test]
fn conflicting_contract_versions_pause_dependent_slices_until_manager_resolution(
) -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let first = fixture.mailbox.assign_slice(
        McpPrincipal::Manager,
        assignment(
            fixture.run_id,
            "provider",
            &["src/api.rs"],
            vec![],
            &["response-shape"],
        ),
    )?;
    let second = fixture.mailbox.assign_slice(
        McpPrincipal::Manager,
        assignment(
            fixture.run_id,
            "consumer",
            &["src/ui.rs"],
            vec![first.id],
            &["response-shape"],
        ),
    )?;
    let first_agent = first.agent_id;
    let second_agent = second.agent_id;

    fixture.mailbox.send_message(
        McpPrincipal::Agent {
            run_id: fixture.run_id,
            agent_id: first_agent,
        },
        fixture.run_id,
        MessageRecipient::Manager,
        MessageKind::Contract,
        "The API returns {\"data\": value}.",
        Some("response-shape"),
    )?;
    fixture.mailbox.send_message(
        McpPrincipal::Agent {
            run_id: fixture.run_id,
            agent_id: second_agent,
        },
        fixture.run_id,
        MessageRecipient::Manager,
        MessageKind::Contract,
        "The API returns {\"result\": value}.",
        Some("response-shape"),
    )?;

    let conflict = fixture
        .mailbox
        .get_contract_decision(fixture.run_id, "response-shape")?
        .expect("contract decision exists");
    assert_eq!(conflict.version, 2);
    assert_eq!(conflict.status, ContractStatus::Conflicted);
    assert_eq!(
        fixture
            .mailbox
            .get_assignment(fixture.run_id, first.id)?
            .status,
        SliceStatus::Paused
    );
    assert_eq!(
        fixture
            .mailbox
            .get_assignment(fixture.run_id, second.id)?
            .status,
        SliceStatus::Paused
    );

    let resolved = fixture.mailbox.resolve_contract(
        McpPrincipal::Manager,
        fixture.run_id,
        "response-shape",
        "The API returns {\"data\": value}.",
    )?;
    assert_eq!(resolved.version, 3);
    assert_eq!(resolved.status, ContractStatus::Resolved);
    assert_eq!(
        fixture
            .mailbox
            .get_assignment(fixture.run_id, first.id)?
            .status,
        SliceStatus::Queued
    );
    assert_eq!(
        fixture
            .mailbox
            .get_assignment(fixture.run_id, second.id)?
            .status,
        SliceStatus::Queued
    );
    Ok(())
}

#[test]
fn agent_mcp_tools_cannot_assign_slices_or_claim_manager_authority() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let agent = fixture.agent();
    fixture.add_agent(agent)?;
    let server = FactoryMcpServer::new(
        fixture.mailbox.clone(),
        fixture.registry.clone(),
        McpPrincipal::Agent {
            run_id: fixture.run_id,
            agent_id: agent,
        },
    );

    let listed = server.list_tools();
    assert!(!listed
        .iter()
        .any(|tool| tool["name"] == "factory_assign_slice"));
    let result = server.handle_json_rpc(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "factory_assign_slice",
            "arguments": {
                "run_id": fixture.run_id,
                "assignment_key": "forged-manager-action",
                "objective": "Start unowned work",
                "allowed_paths": ["src/api.rs"],
                "dependency_ids": [],
                "contract_keys": [],
                "acceptance_evidence": "None"
            }
        }
    }))?;
    assert_eq!(result.pointer("/result/isError"), Some(&Value::Bool(true)));
    assert!(fixture.mailbox.list_assignments(fixture.run_id)?.is_empty());
    Ok(())
}

#[test]
fn stdio_mcp_handshake_exposes_only_tools_for_the_bound_principal() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let server = FactoryMcpServer::new(
        fixture.mailbox.clone(),
        fixture.registry.clone(),
        McpPrincipal::Manager,
    );
    let input = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-03-26\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n"
    );
    let mut reader = Cursor::new(input.as_bytes());
    let mut output = Vec::new();
    server.serve_stdio(&mut reader, &mut output)?;

    let responses = String::from_utf8(output)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(responses.len(), 2);
    assert_eq!(
        responses[0]["result"]["serverInfo"]["name"],
        "agentic-factory"
    );
    let names = responses[1]["result"]["tools"]
        .as_array()
        .expect("MCP tools list")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"factory_assign_slice"));
    assert!(!names.contains(&"factory_check_inbox"));
    Ok(())
}

#[test]
#[ignore = "requires a signed-in local Codex CLI and network access"]
fn live_app_server_calls_the_factory_mcp_tool_with_ephemeral_config() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let repo = temp.path().join("repository");
    fs::create_dir_all(&repo)?;
    git(&repo, &["init", "--initial-branch=main"])?;
    fs::write(repo.join("README.md"), "Harmless MCP integration probe.\n")?;

    let ledger_path = temp.path().join("factory.sqlite");
    let ledger = Arc::new(Ledger::open(&ledger_path)?);
    let registry = RepositoryRegistry::new((*ledger).clone());
    registry.register(&repo)?;
    let worktree_root = temp.path().join("worktrees");
    let worktrees = WorktreeManager::new((*ledger).clone(), worktree_root.clone())?;
    let mailbox = Mailbox::new((*ledger).clone(), worktrees);
    let config_metadata_before = codex_config_metadata()?;

    let session_id = SessionId(Uuid::new_v4());
    ledger.append(&Event::new(session_id, EventKind::SessionCreated))?;
    let config = FactoryMcpConfig::manager(
        PathBuf::from(env!("CARGO_BIN_EXE_factory-mcp-server")),
        ledger_path,
        worktree_root,
    );
    let mut runner =
        CodexRunner::start_with_factory_mcp(&repo, Arc::clone(&ledger), session_id, config)?;
    runner.start_turn(
        "Call factory_list_repositories exactly once, then tell me how many repositories it returned. Do not use any other tools.",
    )?;

    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if mailbox.tool_call_count("factory_list_repositories")? == 1 {
            break;
        }
        let snapshot = ledger.snapshot()?;
        if snapshot.sessions[0].process_state == factory_core::SessionProcessState::Failed {
            anyhow::bail!("live MCP probe failed: {snapshot:?}");
        }
        if Instant::now() >= deadline {
            anyhow::bail!("Codex App Server did not call the factory MCP tool before timeout");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    runner.shutdown()?;
    assert_eq!(mailbox.tool_call_count("factory_list_repositories")?, 1);
    assert_eq!(codex_config_metadata()?, config_metadata_before);
    Ok(())
}

fn codex_config_metadata() -> anyhow::Result<Option<(u64, Option<SystemTime>)>> {
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")));
    let Some(config_path) = codex_home.map(|home| home.join("config.toml")) else {
        return Ok(None);
    };
    match fs::metadata(config_path) {
        Ok(metadata) => Ok(Some((metadata.len(), metadata.modified().ok()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
