use crate::{
    ledger::Ledger,
    model::{
        AgentId, AgentMessage, AssignSliceRequest, ContractDecision, ContractStatus, MessageId,
        MessageKind, MessageRecipient, RunId, SliceAssignment, SliceId, SliceStatus, WorktreeId,
        WorktreeState, WorktreeStatus,
    },
    worktrees::WorktreeManager,
    RedactedOutput,
};
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::{
    collections::BTreeSet,
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_MESSAGE_CHARS: usize = 16_000;
const MAX_ASSIGNMENT_KEY_CHARS: usize = 128;
const MAX_CONTRACT_KEY_CHARS: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum McpPrincipal {
    Manager,
    Agent { run_id: RunId, agent_id: AgentId },
}

impl McpPrincipal {
    pub fn participant(self) -> MessageRecipient {
        match self {
            Self::Manager => MessageRecipient::Manager,
            Self::Agent { agent_id, .. } => MessageRecipient::Agent(agent_id),
        }
    }

    pub fn run_id(self) -> Option<RunId> {
        match self {
            Self::Manager => None,
            Self::Agent { run_id, .. } => Some(run_id),
        }
    }
}

#[derive(Clone)]
pub struct Mailbox {
    ledger: Ledger,
    worktrees: WorktreeManager,
}

impl Mailbox {
    pub fn new(ledger: Ledger, worktrees: WorktreeManager) -> Self {
        Self { ledger, worktrees }
    }

    pub fn send_message(
        &self,
        principal: McpPrincipal,
        run_id: RunId,
        to: MessageRecipient,
        kind: MessageKind,
        body: &str,
        contract_key: Option<&str>,
    ) -> Result<AgentMessage> {
        let from = principal.participant();
        if let McpPrincipal::Agent {
            run_id: principal_run,
            agent_id,
        } = principal
        {
            if principal_run != run_id {
                bail!("agent message run does not match its authenticated session");
            }
            if to == MessageRecipient::Agent(agent_id) {
                bail!("an agent cannot send a message to itself");
            }
        }
        match (from, to) {
            (MessageRecipient::Manager, MessageRecipient::Manager) => {
                bail!("manager messages must have an agent recipient")
            }
            (MessageRecipient::Agent(_), MessageRecipient::Manager)
            | (MessageRecipient::Agent(_), MessageRecipient::Agent(_))
            | (MessageRecipient::Manager, MessageRecipient::Agent(_)) => {}
        }
        if (kind == MessageKind::Contract) != contract_key.is_some() {
            bail!("contract messages require a contract key; other messages must omit it");
        }
        if kind == MessageKind::Contract && to != MessageRecipient::Manager {
            bail!("contract proposals must be directed to the manager");
        }
        let contract_key = contract_key.map(validate_contract_key).transpose()?;
        let body = body.trim();
        if body.is_empty() || body.chars().count() > MAX_MESSAGE_CHARS {
            bail!("message body must contain 1 to {MAX_MESSAGE_CHARS} characters");
        }
        let body = RedactedOutput::new(body).as_str().to_owned();
        let id = MessageId::new();
        let created_at_ms = now_ms();

        self.ledger.with_connection(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure_active_run(&transaction, run_id)?;
            ensure_participant_exists(&transaction, run_id, from)?;
            ensure_participant_exists(&transaction, run_id, to)?;

            let contract_version = if let Some(key) = contract_key.as_deref() {
                Some(record_contract_proposal(
                    &transaction,
                    run_id,
                    key,
                    &body,
                    id,
                    created_at_ms,
                )?)
            } else {
                None
            };
            transaction.execute(
                "INSERT INTO agent_messages
                    (id, run_id, from_kind, from_agent_id, to_kind, to_agent_id, kind, body,
                     contract_key, contract_version, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    id.to_string(),
                    run_id.to_string(),
                    participant_kind(from),
                    participant_agent(from).map(|agent| agent.to_string()),
                    participant_kind(to),
                    participant_agent(to).map(|agent| agent.to_string()),
                    message_kind_text(kind),
                    body,
                    contract_key,
                    contract_version,
                    created_at_ms,
                ],
            )?;
            transaction.commit()?;
            Ok(AgentMessage {
                id,
                run_id,
                from,
                to,
                kind,
                body,
                contract_key,
                contract_version,
                created_at_ms,
                acknowledged_at_ms: None,
            })
        })
    }

    /// Returns stable message IDs until the recipient acknowledges them. A retried read may
    /// return an unacknowledged message again, but it never creates another inbox record.
    pub fn deliver_pending(&self, run_id: RunId, agent_id: AgentId) -> Result<Vec<AgentMessage>> {
        self.ledger.with_connection(|connection| {
            ensure_active_agent(connection, run_id, agent_id)?;
            let mut statement = connection.prepare(
                "SELECT id, run_id, from_kind, from_agent_id, to_kind, to_agent_id, kind, body,
                        contract_key, contract_version, created_at_ms, acknowledged_at_ms
                 FROM agent_messages
                 WHERE run_id = ?1 AND to_kind = 'agent' AND to_agent_id = ?2
                   AND acknowledged_at_ms IS NULL
                 ORDER BY created_at_ms, id",
            )?;
            let rows = statement.query_map(
                params![run_id.to_string(), agent_id.to_string()],
                message_from_row,
            )?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .context("reading pending agent messages")
        })
    }

    pub fn acknowledge(
        &self,
        run_id: RunId,
        agent_id: AgentId,
        message_id: MessageId,
    ) -> Result<()> {
        self.ledger.with_connection(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure_active_agent(&transaction, run_id, agent_id)?;
            let changed = transaction.execute(
                "UPDATE agent_messages SET acknowledged_at_ms = ?4
                 WHERE id = ?1 AND run_id = ?2 AND to_kind = 'agent' AND to_agent_id = ?3
                   AND acknowledged_at_ms IS NULL",
                params![
                    message_id.to_string(),
                    run_id.to_string(),
                    agent_id.to_string(),
                    now_ms()
                ],
            )?;
            if changed == 0 {
                let exists: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM agent_messages
                     WHERE id = ?1 AND run_id = ?2 AND to_kind = 'agent' AND to_agent_id = ?3)",
                    params![
                        message_id.to_string(),
                        run_id.to_string(),
                        agent_id.to_string()
                    ],
                    |row| row.get(0),
                )?;
                if !exists {
                    bail!("message is not addressed to this agent in this run");
                }
            }
            transaction.commit()?;
            Ok(())
        })
    }

    pub fn list_messages(&self, run_id: RunId) -> Result<Vec<AgentMessage>> {
        self.ledger.with_connection(|connection| {
            ensure_active_run(connection, run_id)?;
            let mut statement = connection.prepare(
                "SELECT id, run_id, from_kind, from_agent_id, to_kind, to_agent_id, kind, body,
                        contract_key, contract_version, created_at_ms, acknowledged_at_ms
                 FROM agent_messages WHERE run_id = ?1 ORDER BY created_at_ms, id",
            )?;
            let rows = statement.query_map([run_id.to_string()], message_from_row)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .context("reading run messages")
        })
    }

    pub fn assign_slice(
        &self,
        principal: McpPrincipal,
        request: AssignSliceRequest,
    ) -> Result<SliceAssignment> {
        if principal != McpPrincipal::Manager {
            bail!("only the manager can assign slices");
        }
        let request = normalize_assignment(request)?;
        let integration = self.worktrees.integration_worktree(request.run_id)?;
        if integration.state != WorktreeState::Active {
            bail!("slice assignment requires an active run integration worktree");
        }
        match self.worktrees.inspect_worktree(integration.id)? {
            WorktreeStatus::Clean | WorktreeStatus::Dirty => {}
            WorktreeStatus::Busy => bail!("integration worktree has another operation in progress"),
            _ => bail!("run integration worktree requires manager recovery"),
        }
        if let Some(existing) =
            self.find_assignment_by_key(request.run_id, &request.assignment_key)?
        {
            if assignment_matches(&existing, &request) {
                return Ok(existing);
            }
            bail!("assignment key was reused with different slice content");
        }
        self.worktrees
            .ensure_file_scope_available(request.run_id, &request.allowed_paths)?;
        let id = SliceId::new();
        let agent_id = AgentId::new();
        let created_at_ms = now_ms();
        self.ledger.with_connection(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure_active_run(&transaction, request.run_id)?;
            ensure_dependencies_exist(&transaction, request.run_id, &request.dependency_ids)?;
            transaction.execute(
                "INSERT INTO slice_assignments
                    (id, run_id, assignment_key, objective, acceptance_evidence, allowed_paths_json,
                     agent_id, status, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'preparing', ?8)",
                params![
                    id.to_string(),
                    request.run_id.to_string(),
                    request.assignment_key,
                    request.objective,
                    request.acceptance_evidence,
                    serde_json::to_string(&request.allowed_paths)?,
                    agent_id.to_string(),
                    created_at_ms,
                ],
            )?;
            for dependency in &request.dependency_ids {
                transaction.execute(
                    "INSERT INTO slice_dependencies(slice_id, run_id, dependency_slice_id)
                     VALUES (?1, ?2, ?3)",
                    params![
                        id.to_string(),
                        request.run_id.to_string(),
                        dependency.to_string()
                    ],
                )?;
            }
            for key in &request.contract_keys {
                transaction.execute(
                    "INSERT INTO slice_contracts(slice_id, run_id, contract_key)
                     VALUES (?1, ?2, ?3)",
                    params![id.to_string(), request.run_id.to_string(), key],
                )?;
            }
            transaction.commit()?;
            Ok(())
        })?;

        let worktree = match self.worktrees.create_agent_worktree(
            request.run_id,
            agent_id,
            &integration.base_sha,
        ) {
            Ok(worktree) => worktree,
            Err(error) => {
                self.block_assignment(id, &error.to_string())?;
                return Err(error.context("creating the assigned agent worktree"));
            }
        };
        let scope_refs = request
            .allowed_paths
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        if let Err(error) = self
            .worktrees
            .claim_file_scope(request.run_id, agent_id, &scope_refs)
        {
            let _ = self.worktrees.archive_worktree(worktree.id);
            self.block_assignment(id, &error.to_string())?;
            return Err(error.context("claiming the assigned file scope"));
        }

        self.ledger.with_connection(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let status =
                status_for_contracts(&transaction, request.run_id, &request.contract_keys)?;
            let changed = transaction.execute(
                "UPDATE slice_assignments SET worktree_id = ?2, status = ?3
                 WHERE id = ?1 AND status = 'preparing'",
                params![
                    id.to_string(),
                    worktree.id.to_string(),
                    slice_status_text(status)
                ],
            )?;
            if changed != 1 {
                bail!("slice assignment changed during provisioning");
            }
            transaction.commit()?;
            Ok(())
        })?;
        self.get_assignment(request.run_id, id)
    }

    pub fn list_assignments(&self, run_id: RunId) -> Result<Vec<SliceAssignment>> {
        self.ledger.with_connection(|connection| {
            ensure_active_run(connection, run_id)?;
            let mut statement = connection.prepare(
                "SELECT id FROM slice_assignments WHERE run_id = ?1 ORDER BY created_at_ms, id",
            )?;
            let rows = statement.query_map([run_id.to_string()], |row| row.get::<_, String>(0))?;
            let ids = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            ids.into_iter()
                .map(|id| {
                    let id = SliceId(parse_uuid(&id, 0)?);
                    get_assignment_from_connection(connection, run_id, id)
                        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))
                })
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("reading run slice assignments")
        })
    }

    /// Fails closed if the application stopped during worktree provisioning. The associated
    /// filesystem state is left intact for manager review rather than being silently removed.
    pub fn reconcile_interrupted_assignments(&self) -> Result<usize> {
        self.ledger.with_connection(|connection| {
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut statement = transaction.prepare(
                "SELECT id FROM slice_assignments WHERE status = 'preparing'",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            let ids = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            drop(statement);
            for id in &ids {
                transaction.execute(
                    "UPDATE slice_assignments SET status = 'blocked',
                            blocked_reason = 'application restarted during slice provisioning; manager review required'
                     WHERE id = ?1 AND status = 'preparing'",
                    [id],
                )?;
            }
            transaction.commit()?;
            Ok(ids.len())
        })
    }

    pub fn get_assignment(&self, run_id: RunId, id: SliceId) -> Result<SliceAssignment> {
        self.ledger
            .with_connection(|connection| get_assignment_from_connection(connection, run_id, id))
    }

    pub fn get_contract_decision(
        &self,
        run_id: RunId,
        key: &str,
    ) -> Result<Option<ContractDecision>> {
        self.ledger.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT run_id, contract_key, version, body, status, updated_at_ms
                     FROM contract_decisions WHERE run_id = ?1 AND contract_key = ?2",
                    params![run_id.to_string(), key],
                    contract_from_row,
                )
                .optional()
                .context("reading contract decision")
        })
    }

    pub fn resolve_contract(
        &self,
        principal: McpPrincipal,
        run_id: RunId,
        key: &str,
        body: &str,
    ) -> Result<ContractDecision> {
        if principal != McpPrincipal::Manager {
            bail!("only the manager can resolve a contract decision");
        }
        let key = validate_contract_key(key)?;
        let body = body.trim();
        if body.is_empty() || body.chars().count() > MAX_MESSAGE_CHARS {
            bail!("contract decision must contain 1 to {MAX_MESSAGE_CHARS} characters");
        }
        let body = RedactedOutput::new(body).as_str().to_owned();
        self.ledger.with_connection(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure_active_run(&transaction, run_id)?;
            let current = transaction
                .query_row(
                    "SELECT version FROM contract_decisions WHERE run_id = ?1 AND contract_key = ?2",
                    params![run_id.to_string(), key],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .ok_or_else(|| anyhow!("contract decision '{key}' has not been proposed"))?;
            let version = current
                .checked_add(1)
                .ok_or_else(|| anyhow!("contract version exceeded SQLite integer range"))?;
            let updated_at_ms = now_ms();
            transaction.execute(
                "UPDATE contract_decisions SET version = ?3, body = ?4, status = 'resolved',
                        updated_at_ms = ?5
                 WHERE run_id = ?1 AND contract_key = ?2",
                params![run_id.to_string(), key, version, body, updated_at_ms],
            )?;
            transaction.execute(
                "INSERT INTO contract_versions
                    (run_id, contract_key, version, body, status, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, 'resolved', ?5)",
                params![run_id.to_string(), key, version, body, updated_at_ms],
            )?;
            refresh_assignment_statuses(&transaction, run_id)?;
            transaction.commit()?;
            Ok(ContractDecision {
                run_id,
                key,
                version: version as u32,
                body,
                status: ContractStatus::Resolved,
                updated_at_ms,
            })
        })
    }

    pub fn tool_call_count(&self, name: &str) -> Result<u64> {
        self.ledger.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT COUNT(*) FROM mcp_tool_invocations WHERE tool_name = ?1",
                    [name],
                    |row| row.get::<_, i64>(0),
                )
                .map(|count| count.max(0) as u64)
                .context("counting factory MCP tool invocations")
        })
    }

    pub(crate) fn record_tool_invocation(&self, name: &str, principal: McpPrincipal) -> Result<()> {
        self.ledger.with_connection(|connection| {
            let (principal_kind, agent_id) = match principal {
                McpPrincipal::Manager => ("manager", None),
                McpPrincipal::Agent { agent_id, .. } => ("agent", Some(agent_id.to_string())),
            };
            connection.execute(
                "INSERT INTO mcp_tool_invocations(tool_name, principal_kind, agent_id, invoked_at_ms)
                 VALUES (?1, ?2, ?3, ?4)",
                params![name, principal_kind, agent_id, now_ms()],
            )?;
            Ok(())
        })
    }

    fn find_assignment_by_key(
        &self,
        run_id: RunId,
        assignment_key: &str,
    ) -> Result<Option<SliceAssignment>> {
        self.ledger.with_connection(|connection| {
            let id = connection
                .query_row(
                    "SELECT id FROM slice_assignments WHERE run_id = ?1 AND assignment_key = ?2",
                    params![run_id.to_string(), assignment_key],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            id.map(|id| {
                get_assignment_from_connection(connection, run_id, SliceId(parse_uuid(&id, 0)?))
            })
            .transpose()
        })
    }

    fn block_assignment(&self, id: SliceId, reason: &str) -> Result<()> {
        self.ledger.with_connection(|connection| {
            connection.execute(
                "UPDATE slice_assignments SET status = 'blocked', blocked_reason = ?2 WHERE id = ?1",
                params![id.to_string(), RedactedOutput::new(reason).as_str()],
            )?;
            Ok(())
        })
    }
}

fn normalize_assignment(mut request: AssignSliceRequest) -> Result<AssignSliceRequest> {
    request.assignment_key = request.assignment_key.trim().to_owned();
    request.objective = request.objective.trim().to_owned();
    request.acceptance_evidence = request.acceptance_evidence.trim().to_owned();
    if request.assignment_key.is_empty()
        || request.assignment_key.chars().count() > MAX_ASSIGNMENT_KEY_CHARS
    {
        bail!("assignment key must contain 1 to {MAX_ASSIGNMENT_KEY_CHARS} characters");
    }
    if request.objective.is_empty()
        || request.acceptance_evidence.is_empty()
        || request.objective.chars().count() > MAX_MESSAGE_CHARS
        || request.acceptance_evidence.chars().count() > MAX_MESSAGE_CHARS
    {
        bail!("slice objective and acceptance evidence must contain 1 to {MAX_MESSAGE_CHARS} characters");
    }
    request.allowed_paths = request
        .allowed_paths
        .iter()
        .map(|path| crate::worktrees::normalize_scope_path(path))
        .collect::<Result<Vec<_>>>()?;
    if request.allowed_paths.is_empty() {
        bail!("slice assignment must claim at least one allowed path");
    }
    if request.allowed_paths.iter().collect::<BTreeSet<_>>().len() != request.allowed_paths.len() {
        bail!("slice assignment contains duplicate allowed paths");
    }
    request.allowed_paths.sort();
    if request
        .dependency_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .len()
        != request.dependency_ids.len()
    {
        bail!("slice assignment contains duplicate dependencies");
    }
    request.dependency_ids.sort();
    let mut contract_keys = BTreeSet::new();
    request.contract_keys = request
        .contract_keys
        .iter()
        .map(|key| validate_contract_key(key))
        .collect::<Result<Vec<_>>>()?;
    if request
        .contract_keys
        .iter()
        .any(|key| !contract_keys.insert(key.clone()))
    {
        bail!("slice assignment contains duplicate contract keys");
    }
    request.contract_keys.sort();
    Ok(request)
}

fn status_for_contracts(
    connection: &Connection,
    run_id: RunId,
    keys: &[String],
) -> Result<SliceStatus> {
    let mut waiting = false;
    for key in keys {
        let status = connection
            .query_row(
                "SELECT status FROM contract_decisions WHERE run_id = ?1 AND contract_key = ?2",
                params![run_id.to_string(), key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match status.as_deref() {
            Some("conflicted") => return Ok(SliceStatus::Paused),
            Some("resolved") => {}
            _ => waiting = true,
        }
    }
    Ok(if waiting {
        SliceStatus::WaitingForContract
    } else {
        SliceStatus::Queued
    })
}

fn assignment_matches(existing: &SliceAssignment, request: &AssignSliceRequest) -> bool {
    existing.assignment_key == request.assignment_key
        && existing.objective == request.objective
        && existing.acceptance_evidence == request.acceptance_evidence
        && existing.allowed_paths == request.allowed_paths
        && existing.dependency_ids == request.dependency_ids
        && existing.contract_keys == request.contract_keys
}

fn ensure_active_run(connection: &Connection, run_id: RunId) -> Result<()> {
    let state = connection
        .query_row(
            "SELECT state FROM worktrees WHERE run_id = ?1 AND role_key = 'integration'",
            [run_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("run {run_id} has no integration worktree"))?;
    if state != "active" {
        bail!("run {run_id} integration worktree is not active");
    }
    Ok(())
}

fn ensure_active_agent(connection: &Connection, run_id: RunId, agent_id: AgentId) -> Result<()> {
    let active: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM worktrees WHERE run_id = ?1 AND role_kind = 'agent'
          AND agent_id = ?2 AND state = 'active')",
        params![run_id.to_string(), agent_id.to_string()],
        |row| row.get(0),
    )?;
    if !active {
        bail!("agent {agent_id} has no active worktree in run {run_id}");
    }
    Ok(())
}

fn ensure_participant_exists(
    connection: &Connection,
    run_id: RunId,
    participant: MessageRecipient,
) -> Result<()> {
    match participant {
        MessageRecipient::Manager => ensure_active_run(connection, run_id),
        MessageRecipient::Agent(agent_id) => ensure_active_agent(connection, run_id, agent_id),
    }
}

fn ensure_dependencies_exist(
    connection: &Connection,
    run_id: RunId,
    dependencies: &[SliceId],
) -> Result<()> {
    for dependency in dependencies {
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM slice_assignments WHERE id = ?1 AND run_id = ?2)",
            params![dependency.to_string(), run_id.to_string()],
            |row| row.get(0),
        )?;
        if !exists {
            bail!("dependency slice {dependency} does not belong to run {run_id}");
        }
    }
    Ok(())
}

fn record_contract_proposal(
    transaction: &rusqlite::Transaction<'_>,
    run_id: RunId,
    key: &str,
    body: &str,
    message_id: MessageId,
    now: i64,
) -> Result<u32> {
    let current = transaction
        .query_row(
            "SELECT version, body, status FROM contract_decisions
             WHERE run_id = ?1 AND contract_key = ?2",
            params![run_id.to_string(), key],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let (version, status) = match current {
        None => {
            transaction.execute(
                "INSERT INTO contract_decisions(run_id, contract_key, version, body, status, updated_at_ms)
                 VALUES (?1, ?2, 1, ?3, 'proposed', ?4)",
                params![run_id.to_string(), key, body, now],
            )?;
            (1, ContractStatus::Proposed)
        }
        Some((version, current_body, status)) if current_body == body => (
            version,
            parse_contract_status(&status).ok_or_else(|| anyhow!("invalid contract status"))?,
        ),
        Some((version, _, _)) => {
            let version = version
                .checked_add(1)
                .ok_or_else(|| anyhow!("contract version exceeded SQLite integer range"))?;
            transaction.execute(
                "UPDATE contract_decisions SET version = ?3, body = ?4, status = 'conflicted', updated_at_ms = ?5
                 WHERE run_id = ?1 AND contract_key = ?2",
                params![run_id.to_string(), key, version, body, now],
            )?;
            (version, ContractStatus::Conflicted)
        }
    };
    if !matches!(status, ContractStatus::Resolved) {
        transaction.execute(
            "INSERT INTO contract_versions(run_id, contract_key, version, body, status, source_message_id, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(run_id, contract_key, version) DO NOTHING",
            params![run_id.to_string(), key, version, body, contract_status_text(status), message_id.to_string(), now],
        )?;
    }
    refresh_assignment_statuses(transaction, run_id)?;
    u32::try_from(version).context("contract version exceeded supported range")
}

fn refresh_assignment_statuses(
    transaction: &rusqlite::Transaction<'_>,
    run_id: RunId,
) -> Result<()> {
    let mut statement = transaction.prepare(
        "SELECT id FROM slice_assignments WHERE run_id = ?1 AND status IN ('waiting_for_contract', 'queued', 'paused')",
    )?;
    let rows = statement.query_map([run_id.to_string()], |row| row.get::<_, String>(0))?;
    let ids = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for id in ids {
        let conflict: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM slice_contracts sc JOIN contract_decisions cd
                 ON cd.run_id = sc.run_id AND cd.contract_key = sc.contract_key
               WHERE sc.slice_id = ?1 AND sc.run_id = ?2 AND cd.status = 'conflicted')",
            params![id, run_id.to_string()],
            |row| row.get(0),
        )?;
        let waiting: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM slice_contracts sc LEFT JOIN contract_decisions cd
                 ON cd.run_id = sc.run_id AND cd.contract_key = sc.contract_key
               WHERE sc.slice_id = ?1 AND sc.run_id = ?2 AND (cd.status IS NULL OR cd.status = 'proposed'))",
            params![id, run_id.to_string()],
            |row| row.get(0),
        )?;
        let status = if conflict {
            "paused"
        } else if waiting {
            "waiting_for_contract"
        } else {
            "queued"
        };
        transaction.execute(
            "UPDATE slice_assignments SET status = ?2 WHERE id = ?1",
            params![id, status],
        )?;
    }
    Ok(())
}

fn get_assignment_from_connection(
    connection: &Connection,
    run_id: RunId,
    id: SliceId,
) -> Result<SliceAssignment> {
    let row = connection
        .query_row(
            "SELECT id, run_id, assignment_key, objective, acceptance_evidence, allowed_paths_json,
                    agent_id, worktree_id, status, blocked_reason, created_at_ms
             FROM slice_assignments WHERE id = ?1 AND run_id = ?2",
            params![id.to_string(), run_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, i64>(10)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| anyhow!("slice {id} does not belong to run {run_id}"))?;
    let dependencies =
        query_ids::<SliceId>(connection, "slice_dependencies", "dependency_slice_id", id)?;
    let contract_keys = query_strings(connection, "slice_contracts", "contract_key", id)?;
    Ok(SliceAssignment {
        id: SliceId(parse_uuid(&row.0, 0)?),
        run_id: RunId(parse_uuid(&row.1, 1)?),
        assignment_key: row.2,
        objective: row.3,
        acceptance_evidence: row.4,
        allowed_paths: serde_json::from_str(&row.5).context("invalid allowed paths JSON")?,
        dependency_ids: dependencies,
        contract_keys,
        agent_id: AgentId(parse_uuid(&row.6, 6)?),
        worktree_id: row
            .7
            .map(|id| parse_uuid(&id, 7).map(WorktreeId))
            .transpose()?,
        status: parse_slice_status(&row.8).ok_or_else(|| anyhow!("invalid slice status"))?,
        blocked_reason: row.9,
        created_at_ms: row.10,
    })
}

fn query_ids<T>(
    connection: &Connection,
    table: &str,
    id_column: &str,
    slice_id: SliceId,
) -> Result<Vec<T>>
where
    T: From<uuid::Uuid>,
{
    let sql = format!("SELECT {id_column} FROM {table} WHERE slice_id = ?1 ORDER BY {id_column}");
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([slice_id.to_string()], |row| row.get::<_, String>(0))?;
    rows.map(|row| {
        let raw = row?;
        uuid::Uuid::parse_str(&raw).map(T::from).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
    })
    .collect::<rusqlite::Result<Vec<_>>>()
    .context("reading slice relations")
}

fn query_strings(
    connection: &Connection,
    table: &str,
    value_column: &str,
    slice_id: SliceId,
) -> Result<Vec<String>> {
    let sql =
        format!("SELECT {value_column} FROM {table} WHERE slice_id = ?1 ORDER BY {value_column}");
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([slice_id.to_string()], |row| row.get(0))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("reading slice contract relations")
}

fn message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentMessage> {
    let id: String = row.get(0)?;
    let run_id: String = row.get(1)?;
    let from_kind: String = row.get(2)?;
    let from_agent: Option<String> = row.get(3)?;
    let to_kind: String = row.get(4)?;
    let to_agent: Option<String> = row.get(5)?;
    let kind: String = row.get(6)?;
    let contract_version: Option<i64> = row.get(9)?;
    Ok(AgentMessage {
        id: MessageId(parse_uuid(&id, 0)?),
        run_id: RunId(parse_uuid(&run_id, 1)?),
        from: parse_participant(&from_kind, from_agent, 2)?,
        to: parse_participant(&to_kind, to_agent, 4)?,
        kind: parse_message_kind(&kind).ok_or_else(|| {
            rusqlite::Error::InvalidColumnType(6, "kind".to_owned(), rusqlite::types::Type::Text)
        })?,
        body: row.get(7)?,
        contract_key: row.get(8)?,
        contract_version: contract_version.map(|version| version.max(0) as u32),
        created_at_ms: row.get(10)?,
        acknowledged_at_ms: row.get(11)?,
    })
}

fn contract_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ContractDecision> {
    let run_id: String = row.get(0)?;
    let version: i64 = row.get(2)?;
    let status: String = row.get(4)?;
    Ok(ContractDecision {
        run_id: RunId(parse_uuid(&run_id, 0)?),
        key: row.get(1)?,
        version: version.max(0) as u32,
        body: row.get(3)?,
        status: parse_contract_status(&status).ok_or_else(|| {
            rusqlite::Error::InvalidColumnType(4, "status".to_owned(), rusqlite::types::Type::Text)
        })?,
        updated_at_ms: row.get(5)?,
    })
}

fn parse_participant(
    kind: &str,
    agent_id: Option<String>,
    column: usize,
) -> rusqlite::Result<MessageRecipient> {
    match (kind, agent_id) {
        ("manager", None) => Ok(MessageRecipient::Manager),
        ("agent", Some(id)) => {
            parse_uuid(&id, column).map(|id| MessageRecipient::Agent(AgentId(id)))
        }
        _ => Err(rusqlite::Error::InvalidColumnType(
            column,
            "participant".to_owned(),
            rusqlite::types::Type::Text,
        )),
    }
}

fn participant_kind(participant: MessageRecipient) -> &'static str {
    match participant {
        MessageRecipient::Manager => "manager",
        MessageRecipient::Agent(_) => "agent",
    }
}

fn participant_agent(participant: MessageRecipient) -> Option<AgentId> {
    match participant {
        MessageRecipient::Manager => None,
        MessageRecipient::Agent(agent_id) => Some(agent_id),
    }
}

fn message_kind_text(kind: MessageKind) -> &'static str {
    match kind {
        MessageKind::Question => "question",
        MessageKind::Answer => "answer",
        MessageKind::Handoff => "handoff",
        MessageKind::Contract => "contract",
        MessageKind::Blocker => "blocker",
        MessageKind::Completion => "completion",
    }
}

fn parse_message_kind(kind: &str) -> Option<MessageKind> {
    match kind {
        "question" => Some(MessageKind::Question),
        "answer" => Some(MessageKind::Answer),
        "handoff" => Some(MessageKind::Handoff),
        "contract" => Some(MessageKind::Contract),
        "blocker" => Some(MessageKind::Blocker),
        "completion" => Some(MessageKind::Completion),
        _ => None,
    }
}

fn slice_status_text(status: SliceStatus) -> &'static str {
    match status {
        SliceStatus::Preparing => "preparing",
        SliceStatus::WaitingForContract => "waiting_for_contract",
        SliceStatus::Queued => "queued",
        SliceStatus::Paused => "paused",
        SliceStatus::Blocked => "blocked",
    }
}

fn parse_slice_status(status: &str) -> Option<SliceStatus> {
    match status {
        "preparing" => Some(SliceStatus::Preparing),
        "waiting_for_contract" => Some(SliceStatus::WaitingForContract),
        "queued" => Some(SliceStatus::Queued),
        "paused" => Some(SliceStatus::Paused),
        "blocked" => Some(SliceStatus::Blocked),
        _ => None,
    }
}

fn contract_status_text(status: ContractStatus) -> &'static str {
    match status {
        ContractStatus::Proposed => "proposed",
        ContractStatus::Conflicted => "conflicted",
        ContractStatus::Resolved => "resolved",
    }
}

fn parse_contract_status(status: &str) -> Option<ContractStatus> {
    match status {
        "proposed" => Some(ContractStatus::Proposed),
        "conflicted" => Some(ContractStatus::Conflicted),
        "resolved" => Some(ContractStatus::Resolved),
        _ => None,
    }
}

fn validate_contract_key(key: &str) -> Result<String> {
    let key = key.trim();
    if key.is_empty() || key.chars().count() > MAX_CONTRACT_KEY_CHARS {
        bail!("contract key must contain 1 to {MAX_CONTRACT_KEY_CHARS} characters");
    }
    Ok(key.to_owned())
}

fn parse_uuid(value: &str, column: usize) -> rusqlite::Result<uuid::Uuid> {
    uuid::Uuid::parse_str(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
