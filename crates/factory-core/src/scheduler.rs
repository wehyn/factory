use crate::{
    codex::CodexRunner,
    ledger::Ledger,
    mailbox::Mailbox,
    mcp::FactoryMcpConfig,
    model::{
        AgentId, Event, EventKind, IntegrationRecord, RunId, SchedulerBlocker, SessionId,
        SessionProcessState, SliceAssignment, SliceId, SliceStatus, WorkerExit, Worktree,
        WorktreeStatus,
    },
    worktrees::WorktreeManager,
    RedactedOutput,
};
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub const MAX_SLICE_ATTEMPTS: u32 = 2;
const MAX_EVIDENCE_CHARS: usize = 16_000;

pub trait WorkerProcess: Send {
    fn shutdown(&mut self) -> Result<()>;
    fn poll_exit(&mut self) -> Result<Option<WorkerExit>>;
}

pub trait WorkerLauncher: Send + Sync {
    fn launch(
        &self,
        assignment: &SliceAssignment,
        worktree: &Worktree,
        session_id: SessionId,
    ) -> Result<Box<dyn WorkerProcess>>;
}

#[derive(Clone)]
pub struct CodexWorkerLauncher {
    ledger: Arc<Ledger>,
    worktree_root: PathBuf,
    mcp_server_command: PathBuf,
    mcp_server_args: Vec<String>,
}

impl CodexWorkerLauncher {
    pub fn new(
        ledger: Arc<Ledger>,
        worktrees: &WorktreeManager,
        mcp_server_command: PathBuf,
        mcp_server_args: Vec<String>,
    ) -> Self {
        Self {
            ledger,
            worktree_root: worktrees.root().to_path_buf(),
            mcp_server_command,
            mcp_server_args,
        }
    }
}

impl WorkerLauncher for CodexWorkerLauncher {
    fn launch(
        &self,
        assignment: &SliceAssignment,
        worktree: &Worktree,
        session_id: SessionId,
    ) -> Result<Box<dyn WorkerProcess>> {
        let config = FactoryMcpConfig::agent(
            self.mcp_server_command.clone(),
            self.mcp_server_args.clone(),
            self.ledger.database_path().to_path_buf(),
            self.worktree_root.clone(),
            assignment.run_id,
            assignment.agent_id,
        );
        let mut runner = CodexRunner::start_with_factory_mcp(
            &worktree.path,
            Arc::clone(&self.ledger),
            session_id,
            config,
        )?;
        let prompt = worker_prompt(assignment, worktree);
        if let Err(error) = runner.start_turn_in_worktree(&prompt, &worktree.path) {
            let _ = runner.shutdown();
            return Err(error.context("starting the bounded worker turn"));
        }
        Ok(Box::new(CodexWorkerProcess {
            runner,
            ledger: Arc::clone(&self.ledger),
            session_id,
        }))
    }
}

struct CodexWorkerProcess {
    runner: CodexRunner,
    ledger: Arc<Ledger>,
    session_id: SessionId,
}

impl WorkerProcess for CodexWorkerProcess {
    fn shutdown(&mut self) -> Result<()> {
        self.runner.shutdown()
    }

    fn poll_exit(&mut self) -> Result<Option<WorkerExit>> {
        match self.runner.try_wait_for_exit() {
            Ok(None) => Ok(None),
            Err(_) => Ok(Some(WorkerExit::Failed)),
            Ok(Some(_)) => {
                let session = self
                    .ledger
                    .snapshot()?
                    .sessions
                    .into_iter()
                    .find(|session| session.session_id == self.session_id)
                    .ok_or_else(|| anyhow!("worker session disappeared from the ledger"))?;
                Ok(Some(match session.process_state {
                    SessionProcessState::Completed => WorkerExit::Completed,
                    SessionProcessState::Interrupted => WorkerExit::Interrupted,
                    SessionProcessState::Starting
                    | SessionProcessState::Running
                    | SessionProcessState::Failed => WorkerExit::Failed,
                }))
            }
        }
    }
}

#[derive(Clone)]
pub struct Scheduler {
    ledger: Arc<Ledger>,
    worktrees: WorktreeManager,
    mailbox: Mailbox,
    launcher: Arc<dyn WorkerLauncher>,
    workers: Arc<Mutex<HashMap<AgentId, Box<dyn WorkerProcess>>>>,
}

impl Scheduler {
    pub fn control(ledger: Arc<Ledger>, worktrees: WorktreeManager) -> Self {
        Self::new(ledger, worktrees, Arc::new(DisabledWorkerLauncher))
    }

    pub fn new(
        ledger: Arc<Ledger>,
        worktrees: WorktreeManager,
        launcher: Arc<dyn WorkerLauncher>,
    ) -> Self {
        let mailbox = Mailbox::new((*ledger).clone(), worktrees.clone());
        Self {
            ledger,
            worktrees,
            mailbox,
            launcher,
            workers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn ready(&self, run_id: RunId) -> Result<Vec<SliceId>> {
        let assignments = self.mailbox.list_assignments(run_id)?;
        let integration = self.worktrees.integration_worktree(run_id)?;
        match self.worktrees.inspect_worktree(integration.id)? {
            WorktreeStatus::Clean => {}
            WorktreeStatus::Busy => return Ok(Vec::new()),
            status => {
                let detail = format!(
                    "integration worktree is {status:?}; manager recovery is required before starting workers"
                );
                for assignment in assignments
                    .iter()
                    .filter(|assignment| assignment.status == SliceStatus::Queued)
                {
                    self.block_slice(
                        assignment.id,
                        run_id,
                        "integration_worktree_unavailable",
                        &detail,
                    )?;
                }
                return Ok(Vec::new());
            }
        }
        let mut ready = Vec::new();
        for assignment in assignments {
            if assignment.status != SliceStatus::Queued {
                continue;
            }
            let mut dependencies_ready = true;
            for dependency in &assignment.dependency_ids {
                let dependency = self.mailbox.get_assignment(run_id, *dependency)?;
                if dependency.status != SliceStatus::Integrated {
                    dependencies_ready = false;
                    break;
                }
            }
            if !dependencies_ready {
                continue;
            }
            let mut contracts_ready = true;
            for key in &assignment.contract_keys {
                match self.mailbox.get_contract_decision(run_id, key)? {
                    Some(decision) if decision.status == crate::model::ContractStatus::Resolved => {
                    }
                    _ => {
                        contracts_ready = false;
                        break;
                    }
                }
            }
            if !contracts_ready {
                continue;
            }
            let Some(worktree_id) = assignment.worktree_id else {
                self.block_slice(
                    assignment.id,
                    run_id,
                    "missing_worktree",
                    "queued slice has no registered agent worktree",
                )?;
                continue;
            };
            match self.worktrees.inspect_worktree(worktree_id)? {
                WorktreeStatus::Clean => ready.push(assignment.id),
                WorktreeStatus::Busy => {}
                status => {
                    let detail = format!(
                        "queued worker worktree is {status:?}; manager recovery is required"
                    );
                    self.block_slice(assignment.id, run_id, "unsafe_worktree", &detail)?;
                }
            }
        }
        ready.sort();
        Ok(ready)
    }

    /// Atomically claims and launches one ready slice. A durable running row is written before
    /// spawning; retries or a second scheduler instance cannot launch a duplicate process.
    pub fn spawn_slice(&self, slice_id: SliceId) -> Result<AgentId> {
        let assignment = self.find_assignment(slice_id)?;
        if assignment.status != SliceStatus::Queued {
            bail!("slice is not queued; the prior process may still own it");
        }
        let worktree_id = assignment
            .worktree_id
            .ok_or_else(|| anyhow!("slice has no agent worktree"))?;
        let worktree = self
            .worktrees
            .advance_clean_agent_worktree_to_integration(worktree_id)?;
        if self.worktrees.inspect_worktree(worktree_id)? != WorktreeStatus::Clean {
            bail!("slice worktree is not clean and ready for a worker");
        }
        let session_id = SessionId(Uuid::new_v4());
        let attempt_id = Uuid::new_v4();
        let attempt_number = self.ledger.with_connection(|connection| {
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row = transaction.query_row(
                "SELECT run_id, agent_id, status, attempt_count FROM slice_assignments WHERE id = ?1",
                [slice_id.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, i64>(3)?)),
            ).optional()?.ok_or_else(|| anyhow!("slice {slice_id} is not assigned"))?;
            if row.0 != assignment.run_id.to_string() || row.1 != assignment.agent_id.to_string() {
                bail!("slice identity changed before worker launch");
            }
            if row.2 != "queued" {
                bail!("slice is not queued; the prior process may still own it");
            }
            ensure_active_run(&transaction, assignment.run_id)?;
            let current_attempt = u32::try_from(row.3.max(0)).unwrap_or(u32::MAX);
            if current_attempt >= MAX_SLICE_ATTEMPTS {
                bail!("slice exhausted its bounded retry allowance");
            }
            ensure_dependencies_integrated_or_completed(&transaction, slice_id, assignment.run_id)?;
            ensure_contracts_resolved(&transaction, slice_id, assignment.run_id)?;
            let attempt_number = current_attempt + 1;
            let changed = transaction.execute(
                "UPDATE slice_assignments SET status = 'running', attempt_count = ?2, blocked_reason = NULL
                 WHERE id = ?1 AND status = 'queued'",
                params![slice_id.to_string(), attempt_number],
            )?;
            if changed != 1 {
                bail!("slice was claimed by another scheduler");
            }
            transaction.execute(
                "INSERT INTO slice_attempts
                    (id, slice_id, run_id, agent_id, attempt_number, session_id, state, started_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'starting', ?7)",
                params![attempt_id.to_string(), slice_id.to_string(), assignment.run_id.to_string(),
                    assignment.agent_id.to_string(), attempt_number, session_id.to_string(), now_ms()],
            )?;
            transaction.commit()?;
            Ok(attempt_number)
        })?;

        if let Err(error) = self
            .ledger
            .append(&Event::new(session_id, EventKind::SessionCreated))
        {
            let _ = self.process_exited(assignment.agent_id, WorkerExit::Failed);
            return Err(error.context("recording the worker session before launch"));
        }
        let mut worker = match self.launcher.launch(&assignment, &worktree, session_id) {
            Ok(worker) => worker,
            Err(error) => {
                let _ = self.process_exited(assignment.agent_id, WorkerExit::Failed);
                return Err(error.context(format!("launching slice attempt {attempt_number}")));
            }
        };
        let mut processes = self
            .workers
            .lock()
            .map_err(|_| anyhow!("worker process registry lock was poisoned"))?;
        if processes.contains_key(&assignment.agent_id) {
            let _ = worker.shutdown();
            drop(processes);
            let _ = self.process_exited(assignment.agent_id, WorkerExit::Failed);
            bail!("worker process registry already contains this agent");
        }
        processes.insert(assignment.agent_id, worker);
        self.ledger.with_connection(|connection| {
            connection.execute(
                "UPDATE slice_attempts SET state = 'running' WHERE id = ?1",
                [attempt_id.to_string()],
            )?;
            Ok(())
        })?;
        Ok(assignment.agent_id)
    }

    pub fn dispatch_ready(&self, run_id: RunId) -> Result<Vec<AgentId>> {
        let mut agents = Vec::new();
        for slice_id in self.ready(run_id)? {
            match self.spawn_slice(slice_id) {
                Ok(agent_id) => agents.push(agent_id),
                // Pre-claim failures such as a temporary worktree lock leave the assignment
                // queued for the next monitor pass. Post-claim process failures persist their
                // own retry state and blocker through `process_exited`.
                Err(_) => {}
            }
        }
        Ok(agents)
    }

    pub fn dispatch_all_ready(&self) -> Result<Vec<AgentId>> {
        let runs = self.ledger.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT DISTINCT run_id FROM slice_assignments WHERE status = 'queued' ORDER BY run_id",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            rows.map(|row| {
                let run = row?;
                uuid::Uuid::parse_str(&run).map(RunId).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
                })
            })
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("listing scheduler runs")
        })?;
        let mut agents = Vec::new();
        for run_id in runs {
            agents.extend(self.dispatch_ready(run_id)?);
        }
        Ok(agents)
    }

    pub fn complete_slice(&self, agent_id: AgentId, evidence: &str) -> Result<SliceAssignment> {
        let evidence = evidence.trim();
        if evidence.is_empty() || evidence.chars().count() > MAX_EVIDENCE_CHARS {
            bail!("completion evidence must contain 1 to {MAX_EVIDENCE_CHARS} characters");
        }
        let evidence = RedactedOutput::new(evidence).as_str().to_owned();
        let assignment = self.find_assignment_for_agent(agent_id)?;
        if matches!(
            assignment.status,
            SliceStatus::Completed | SliceStatus::Integrated
        ) {
            if assignment.completion_evidence.as_deref() == Some(evidence.as_str()) {
                return Ok(assignment);
            }
            bail!("slice was already completed with different evidence");
        }
        if assignment.status != SliceStatus::Running {
            bail!("only a running slice can be completed");
        }
        let attempt = self.latest_attempt(assignment.id)?;
        let session = self
            .ledger
            .snapshot()?
            .sessions
            .into_iter()
            .find(|session| session.session_id == attempt.0)
            .ok_or_else(|| anyhow!("worker session is missing from the ledger"))?;
        if session.process_state != SessionProcessState::Completed {
            bail!("worker must finish its App Server turn before completion is accepted");
        }
        let worktree_id = assignment
            .worktree_id
            .ok_or_else(|| anyhow!("slice has no agent worktree"))?;
        let validation = self
            .worktrees
            .with_exclusive_worktree(worktree_id, |worktree, status| {
                if !matches!(status, WorktreeStatus::Clean | WorktreeStatus::Dirty) {
                    bail!("worker worktree is unavailable; manager review is required");
                }
                capture_scoped_worker_changes(worktree, &assignment)
            });
        let source_commit = match validation {
            Ok(source_commit) => source_commit,
            Err(error) => {
                self.block_slice(
                    assignment.id,
                    assignment.run_id,
                    "invalid_completion",
                    &error.to_string(),
                )?;
                return Err(error);
            }
        };

        self.ledger.with_connection(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let changed = transaction.execute(
                "UPDATE slice_assignments SET status = 'completed', source_commit = ?2,
                        completion_evidence = ?3, blocked_reason = NULL
                 WHERE id = ?1 AND status = 'running' AND agent_id = ?4",
                params![
                    assignment.id.to_string(),
                    source_commit,
                    evidence,
                    agent_id.to_string()
                ],
            )?;
            if changed != 1 {
                bail!("slice state changed before completion could be recorded");
            }
            transaction.execute(
                "UPDATE slice_attempts SET state = 'completed', finished_at_ms = ?2, detail = ?3
                 WHERE id = ?1",
                params![attempt.1.to_string(), now_ms(), evidence],
            )?;
            transaction.commit()?;
            Ok(())
        })?;
        self.stop_worker(agent_id)?;
        self.mailbox
            .get_assignment(assignment.run_id, assignment.id)
    }

    pub fn process_exited(&self, agent_id: AgentId, exit: WorkerExit) -> Result<()> {
        let assignment = self.find_assignment_for_agent(agent_id)?;
        if matches!(
            assignment.status,
            SliceStatus::Completed | SliceStatus::Integrated
        ) {
            self.stop_worker(agent_id)?;
            return Ok(());
        }
        if assignment.status != SliceStatus::Running {
            self.stop_worker(agent_id)?;
            return Ok(());
        }
        let (session_id, attempt_id, attempt_number) = self.latest_attempt(assignment.id)?;
        let safe_worktree = match assignment.worktree_id {
            Some(id) if self.worktrees.inspect_worktree(id)? == WorktreeStatus::Clean => {
                let worktree = self.worktrees.get_worktree(id)?;
                git_text(&worktree.path, &["rev-parse", "HEAD"])? == worktree.base_sha
            }
            _ => false,
        };
        let retryable =
            exit != WorkerExit::Completed && safe_worktree && attempt_number < MAX_SLICE_ATTEMPTS;
        let status = if retryable { "retryable" } else { "blocked" };
        let detail = match (exit, safe_worktree, retryable) {
            (WorkerExit::Completed, _, _) => "worker process exited without manager completion evidence".to_owned(),
            (_, false, _) => "worker process exited with a dirty or uncertain worktree; manager recovery is required".to_owned(),
            (_, true, true) => format!("worker attempt {attempt_number} ended and is eligible for one bounded retry"),
            (_, true, false) => format!("worker exhausted the {MAX_SLICE_ATTEMPTS}-attempt limit"),
        };
        self.ledger.with_connection(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute(
                "UPDATE slice_assignments SET status = ?2, blocked_reason = ?3
                 WHERE id = ?1 AND status = 'running'",
                params![
                    assignment.id.to_string(),
                    status,
                    RedactedOutput::new(&detail).as_str()
                ],
            )?;
            transaction.execute(
                "UPDATE slice_attempts SET state = ?2, finished_at_ms = ?3, detail = ?4
                 WHERE id = ?1 AND state IN ('starting', 'running')",
                params![
                    attempt_id.to_string(),
                    exit_text(exit),
                    now_ms(),
                    RedactedOutput::new(&detail).as_str()
                ],
            )?;
            if !retryable {
                insert_blocker(
                    &transaction,
                    assignment.run_id,
                    Some(assignment.id),
                    "worker_exit",
                    &detail,
                )?;
            }
            transaction.commit()?;
            Ok(())
        })?;
        let _ = session_id;
        self.stop_worker(agent_id)
    }

    pub fn retry_slice(&self, slice_id: SliceId) -> Result<()> {
        let assignment = self.find_assignment(slice_id)?;
        if assignment.status == SliceStatus::Queued {
            return Ok(());
        }
        if assignment.status != SliceStatus::Retryable {
            bail!("slice is not eligible for retry");
        }
        if assignment.attempt_count >= MAX_SLICE_ATTEMPTS {
            bail!("slice exhausted its bounded retry allowance");
        }
        let worktree_id = assignment
            .worktree_id
            .ok_or_else(|| anyhow!("slice has no worktree"))?;
        let worktree = self.worktrees.get_worktree(worktree_id)?;
        if self.worktrees.inspect_worktree(worktree_id)? != WorktreeStatus::Clean
            || git_text(&worktree.path, &["rev-parse", "HEAD"])? != worktree.base_sha
        {
            self.block_slice(
                slice_id,
                assignment.run_id,
                "dirty_retry_worktree",
                "retry refused because the prior worker left uncommitted or unsafe files",
            )?;
            bail!("retry refused because the worker worktree is not clean");
        }
        self.ledger.with_connection(|connection| {
            let changed = connection.execute(
                "UPDATE slice_assignments SET status = 'queued', blocked_reason = NULL
                 WHERE id = ?1 AND status = 'retryable' AND attempt_count < ?2",
                params![slice_id.to_string(), MAX_SLICE_ATTEMPTS],
            )?;
            if changed != 1 {
                bail!("slice retry state changed before it could be queued");
            }
            Ok(())
        })
    }

    pub fn integration_ready(&self, run_id: RunId) -> Result<bool> {
        self.ledger.with_connection(|connection| {
            let total: i64 = connection.query_row(
                "SELECT COUNT(*) FROM slice_assignments WHERE run_id = ?1",
                [run_id.to_string()],
                |row| row.get(0),
            )?;
            if total == 0 {
                return Ok(false);
            }
            let open: i64 = connection.query_row(
                "SELECT COUNT(*) FROM slice_assignments
                 WHERE run_id = ?1 AND status NOT IN ('completed', 'integrated')",
                [run_id.to_string()],
                |row| row.get(0),
            )?;
            let blockers: i64 = connection.query_row(
                "SELECT COUNT(*) FROM scheduler_blockers WHERE run_id = ?1 AND resolved_at_ms IS NULL",
                [run_id.to_string()],
                |row| row.get(0),
            )?;
            Ok(open == 0 && blockers == 0)
        })
    }

    pub fn integrate_completed(&self, run_id: RunId) -> Result<Vec<IntegrationRecord>> {
        self.ledger.with_connection(|connection| {
            ensure_active_run(connection, run_id)?;
            let blockers: i64 = connection.query_row(
                "SELECT COUNT(*) FROM scheduler_blockers WHERE run_id = ?1 AND resolved_at_ms IS NULL",
                [run_id.to_string()],
                |row| row.get(0),
            )?;
            if blockers > 0 {
                bail!("run has unresolved manager blockers");
            }
            Ok(())
        })?;
        if self
            .integration_records(run_id)?
            .iter()
            .any(|record| record.state == "preparing" || record.state == "blocked")
        {
            bail!("run has an unfinished integration operation; manager recovery is required");
        }
        let mut integrated = Vec::new();
        loop {
            let assignments = self.mailbox.list_assignments(run_id)?;
            let mut candidate = None;
            for assignment in assignments
                .iter()
                .filter(|assignment| assignment.status == SliceStatus::Completed)
            {
                if assignment.dependency_ids.iter().all(|dependency| {
                    self.mailbox
                        .get_assignment(run_id, *dependency)
                        .map(|dependency| dependency.status == SliceStatus::Integrated)
                        .unwrap_or(false)
                }) {
                    candidate = Some(assignment.clone());
                    break;
                }
            }
            let Some(assignment) = candidate else { break };
            match self.integrate_one(&assignment) {
                Ok(record) => integrated.push(record),
                Err(error) => {
                    self.record_integration_blocker(&assignment, &error.to_string())?;
                    return Err(error);
                }
            }
        }
        Ok(integrated)
    }

    fn integrate_one(&self, assignment: &SliceAssignment) -> Result<IntegrationRecord> {
        let source_commit = assignment
            .source_commit
            .as_deref()
            .ok_or_else(|| anyhow!("completed slice has no recorded source commit"))?;
        let source_worktree = self.worktrees.get_worktree(
            assignment
                .worktree_id
                .ok_or_else(|| anyhow!("completed slice has no worker worktree"))?,
        )?;
        let source_base_commit = source_worktree.base_sha.clone();
        self.worktrees
            .with_exclusive_worktree(source_worktree.id, |worktree, status| {
                if status != WorktreeStatus::Clean {
                    bail!("completed source worktree is no longer clean");
                }
                let current = git_text(&worktree.path, &["rev-parse", "HEAD"])?;
                if current != source_commit {
                    bail!("completed worker branch moved after its source commit was recorded");
                }
                ensure_commit_scope(worktree, assignment, source_commit)?;
                Ok(())
            })?;

        let integration = self.worktrees.integration_worktree(assignment.run_id)?;
        let run_id = assignment.run_id;
        let slice_id = assignment.id;
        let operation = self.worktrees.with_exclusive_worktree(integration.id, |worktree, status| {
            if status != WorktreeStatus::Clean {
                bail!("integration worktree has uncommitted changes; manager recovery is required");
            }
            let integration_base = git_text(&worktree.path, &["rev-parse", "HEAD"])?;
            self.ledger.with_connection(|connection| {
                let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let already_integrated: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM slice_integrations WHERE slice_id = ?1)",
                    [slice_id.to_string()],
                    |row| row.get(0),
                )?;
                if already_integrated {
                    bail!("slice already has an integration record; manager recovery is required");
                }
                transaction.execute(
                    "INSERT INTO slice_integrations
                        (slice_id, run_id, source_commit, source_base_commit, integration_base_commit, state)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'preparing')",
                    params![slice_id.to_string(), run_id.to_string(), source_commit, source_base_commit, integration_base],
                )?;
                transaction.commit()?;
                Ok(())
            })?;
            let commit_range = format!("{}..{}", source_base_commit, source_commit);
            if let Err(error) = git_status(
                worktree.path.as_path(),
                &["cherry-pick", "-x", &commit_range],
            ) {
                let _ = git_status(worktree.path.as_path(), &["cherry-pick", "--abort"]);
                return Err(error.context("cherry-picking the completed slice into integration"));
            }
            let destination_commit = git_text(&worktree.path, &["rev-parse", "HEAD"])?;
            let integrated_at_ms = now_ms();
            self.ledger.with_connection(|connection| {
                let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                transaction.execute(
                    "UPDATE slice_integrations SET destination_commit = ?2, state = 'integrated', integrated_at_ms = ?3
                     WHERE slice_id = ?1 AND state = 'preparing'",
                    params![slice_id.to_string(), destination_commit, integrated_at_ms],
                )?;
                transaction.execute(
                    "UPDATE slice_assignments SET status = 'integrated'
                     WHERE id = ?1 AND status = 'completed'",
                    [slice_id.to_string()],
                )?;
                transaction.commit()?;
                Ok(())
            })?;
            Ok(IntegrationRecord {
                slice_id,
                run_id,
                source_commit: source_commit.to_owned(),
                source_base_commit: source_base_commit.clone(),
                integration_base_commit: integration_base,
                destination_commit: Some(destination_commit),
                state: "integrated".to_owned(),
                integrated_at_ms: Some(integrated_at_ms),
            })
        });
        operation
    }

    pub fn integration_records(&self, run_id: RunId) -> Result<Vec<IntegrationRecord>> {
        self.ledger.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT slice_id, run_id, source_commit, source_base_commit, integration_base_commit,
                        destination_commit, state, integrated_at_ms
                 FROM slice_integrations WHERE run_id = ?1 ORDER BY rowid",
            )?;
            let rows = statement.query_map([run_id.to_string()], integration_record_from_row)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .context("reading slice integration records")
        })
    }

    pub fn list_blockers(&self, run_id: RunId) -> Result<Vec<SchedulerBlocker>> {
        self.ledger.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, run_id, slice_id, kind, detail, created_at_ms, resolved_at_ms
                 FROM scheduler_blockers WHERE run_id = ?1 AND resolved_at_ms IS NULL
                 ORDER BY created_at_ms, id",
            )?;
            let rows = statement.query_map([run_id.to_string()], blocker_from_row)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .context("reading scheduler blockers")
        })
    }

    pub fn resolve_blocker(&self, blocker_id: Uuid) -> Result<()> {
        self.ledger.with_connection(|connection| {
            let changed = connection.execute(
                "UPDATE scheduler_blockers SET resolved_at_ms = ?2
                 WHERE id = ?1 AND resolved_at_ms IS NULL",
                params![blocker_id.to_string(), now_ms()],
            )?;
            if changed == 0 {
                let exists: bool = connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM scheduler_blockers WHERE id = ?1)",
                    [blocker_id.to_string()],
                    |row| row.get(0),
                )?;
                if !exists {
                    bail!("scheduler blocker does not exist");
                }
            }
            Ok(())
        })
    }

    pub fn get_assignment(&self, slice_id: SliceId) -> Result<SliceAssignment> {
        self.find_assignment(slice_id)
    }

    pub fn session_for_agent(
        &self,
        agent_id: AgentId,
    ) -> Result<Option<crate::model::SessionSnapshot>> {
        let assignment = self.find_assignment_for_agent(agent_id)?;
        if assignment.attempt_count == 0 {
            return Ok(None);
        }
        let (session_id, _, _) = self.latest_attempt(assignment.id)?;
        Ok(self
            .ledger
            .snapshot()?
            .sessions
            .into_iter()
            .find(|session| session.session_id == session_id))
    }

    pub fn reconcile_after_restart(&self) -> Result<usize> {
        self.ledger.with_connection(|connection| {
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut statement = transaction.prepare(
                "SELECT id, run_id FROM slice_assignments WHERE status = 'running'",
            )?;
            let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
            let running = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            drop(statement);
            for (slice_id, run_id) in &running {
                transaction.execute(
                    "UPDATE slice_assignments SET status = 'blocked',
                            blocked_reason = 'worker process state is uncertain after application restart; manager review required'
                     WHERE id = ?1 AND status = 'running'",
                    [slice_id],
                )?;
                transaction.execute(
                    "UPDATE slice_attempts SET state = 'interrupted', finished_at_ms = ?2,
                            detail = 'worker process state is uncertain after application restart'
                     WHERE slice_id = ?1 AND state IN ('starting', 'running')",
                    params![slice_id, now_ms()],
                )?;
                insert_blocker(
                    &transaction,
                    parse_run_id(run_id)?,
                    Some(parse_slice_id(slice_id)?),
                    "uncertain_worker_process",
                    "worker process state is uncertain after application restart; it will not be relaunched automatically",
                )?;
            }

            let mut statement = transaction.prepare(
                "SELECT slice_id, run_id FROM slice_integrations WHERE state = 'preparing'",
            )?;
            let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
            let integrating = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            drop(statement);
            for (slice_id, run_id) in &integrating {
                transaction.execute(
                    "UPDATE slice_integrations SET state = 'blocked'
                     WHERE slice_id = ?1 AND state = 'preparing'",
                    [slice_id],
                )?;
                transaction.execute(
                    "UPDATE slice_assignments SET status = 'blocked',
                            blocked_reason = 'integration stopped during commit; inspect the integration worktree'
                     WHERE id = ?1",
                    [slice_id],
                )?;
                insert_blocker(
                    &transaction,
                    parse_run_id(run_id)?,
                    Some(parse_slice_id(slice_id)?),
                    "uncertain_integration",
                    "integration stopped between its durable start record and completion; inspect the integration worktree before retrying",
                )?;
            }
            transaction.commit()?;
            Ok(running.len() + integrating.len())
        })
    }

    pub fn poll_workers(&self) -> Result<usize> {
        let (exited, completed) = {
            let mut workers = self
                .workers
                .lock()
                .map_err(|_| anyhow!("worker process registry lock was poisoned"))?;
            let mut exited = Vec::new();
            let mut completed = Vec::new();
            for (agent_id, worker) in workers.iter_mut() {
                match self.find_assignment_for_agent(*agent_id) {
                    Ok(assignment)
                        if matches!(
                            assignment.status,
                            SliceStatus::Completed | SliceStatus::Integrated
                        ) =>
                    {
                        completed.push(*agent_id);
                    }
                    Ok(assignment) if assignment.status == SliceStatus::Running => {
                        let session_state =
                            self.latest_attempt(assignment.id).and_then(|attempt| {
                                self.ledger.snapshot().map(|snapshot| {
                                    snapshot
                                        .sessions
                                        .into_iter()
                                        .find(|session| session.session_id == attempt.0)
                                        .map(|session| session.process_state)
                                })
                            });
                        match session_state {
                            Ok(Some(SessionProcessState::Failed)) => {
                                exited.push((*agent_id, WorkerExit::Failed))
                            }
                            Ok(Some(SessionProcessState::Interrupted)) => {
                                exited.push((*agent_id, WorkerExit::Interrupted))
                            }
                            _ => match worker.poll_exit() {
                                Ok(Some(exit)) => exited.push((*agent_id, exit)),
                                Ok(None) => {}
                                Err(_) => exited.push((*agent_id, WorkerExit::Failed)),
                            },
                        }
                    }
                    _ => match worker.poll_exit() {
                        Ok(Some(exit)) => exited.push((*agent_id, exit)),
                        Ok(None) => {}
                        Err(_) => exited.push((*agent_id, WorkerExit::Failed)),
                    },
                }
            }
            (exited, completed)
        };
        for agent_id in completed {
            self.stop_worker(agent_id)?;
        }
        for (agent_id, exit) in &exited {
            self.process_exited(*agent_id, *exit)?;
        }
        Ok(exited.len())
    }

    pub fn shutdown(&self) -> Result<()> {
        let agents = self
            .workers
            .lock()
            .map_err(|_| anyhow!("worker process registry lock was poisoned"))?
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for agent_id in agents {
            if let Some(mut worker) = self
                .workers
                .lock()
                .map_err(|_| anyhow!("worker process registry lock was poisoned"))?
                .remove(&agent_id)
            {
                let _ = worker.shutdown();
            }
            self.process_exited(agent_id, WorkerExit::Interrupted)?;
        }
        Ok(())
    }

    fn stop_worker(&self, agent_id: AgentId) -> Result<()> {
        let worker = self
            .workers
            .lock()
            .map_err(|_| anyhow!("worker process registry lock was poisoned"))?
            .remove(&agent_id);
        if let Some(mut worker) = worker {
            worker.shutdown()?;
        }
        Ok(())
    }

    fn find_assignment(&self, slice_id: SliceId) -> Result<SliceAssignment> {
        let run_id = self.ledger.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT run_id FROM slice_assignments WHERE id = ?1",
                    [slice_id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| anyhow!("slice {slice_id} is not assigned"))
        })?;
        self.mailbox
            .get_assignment(parse_run_id(&run_id)?, slice_id)
    }

    fn find_assignment_for_agent(&self, agent_id: AgentId) -> Result<SliceAssignment> {
        let slice_id = self.ledger.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT id FROM slice_assignments WHERE agent_id = ?1",
                    [agent_id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| anyhow!("agent {agent_id} has no assigned slice"))
        })?;
        self.find_assignment(parse_slice_id(&slice_id)?)
    }

    fn latest_attempt(&self, slice_id: SliceId) -> Result<(SessionId, Uuid, u32)> {
        self.ledger.with_connection(|connection| {
            let row = connection
                .query_row(
                    "SELECT session_id, id, attempt_number FROM slice_attempts
                 WHERE slice_id = ?1 ORDER BY attempt_number DESC LIMIT 1",
                    [slice_id.to_string()],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()?
                .ok_or_else(|| anyhow!("slice has no recorded worker attempt"))?;
            Ok((
                SessionId(parse_uuid(&row.0)?),
                parse_uuid(&row.1)?,
                u32::try_from(row.2.max(0)).unwrap_or(u32::MAX),
            ))
        })
    }

    fn block_slice(
        &self,
        slice_id: SliceId,
        run_id: RunId,
        kind: &str,
        detail: &str,
    ) -> Result<()> {
        let safe = RedactedOutput::new(detail).as_str().to_owned();
        self.ledger.with_connection(|connection| {
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute(
                "UPDATE slice_assignments SET status = 'blocked', blocked_reason = ?2 WHERE id = ?1",
                params![slice_id.to_string(), safe],
            )?;
            transaction.execute(
                "UPDATE slice_attempts SET state = 'blocked', finished_at_ms = ?2, detail = ?3
                 WHERE slice_id = ?1 AND state IN ('starting', 'running')",
                params![slice_id.to_string(), now_ms(), safe],
            )?;
            insert_blocker(&transaction, run_id, Some(slice_id), kind, &safe)?;
            transaction.commit()?;
            Ok(())
        })
    }

    fn record_integration_blocker(&self, assignment: &SliceAssignment, detail: &str) -> Result<()> {
        let safe = RedactedOutput::new(detail).as_str().to_owned();
        self.ledger.with_connection(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute(
                "UPDATE slice_integrations SET state = 'blocked'
                 WHERE slice_id = ?1 AND state = 'preparing'",
                [assignment.id.to_string()],
            )?;
            insert_blocker(
                &transaction,
                assignment.run_id,
                Some(assignment.id),
                "integration_failed",
                &safe,
            )?;
            transaction.commit()?;
            Ok(())
        })
    }
}

struct DisabledWorkerLauncher;

impl WorkerLauncher for DisabledWorkerLauncher {
    fn launch(
        &self,
        _assignment: &SliceAssignment,
        _worktree: &Worktree,
        _session_id: SessionId,
    ) -> Result<Box<dyn WorkerProcess>> {
        bail!("worker process launch is only available from the resident application service")
    }
}

fn worker_prompt(assignment: &SliceAssignment, worktree: &Worktree) -> String {
    format!(
        "You are a bounded Agentic Factory builder. Work only in this assigned Git worktree and the listed paths. Do not create or delegate work, change files outside scope, merge branches, or contact Wayne directly. Use the factory MCP tools to read your inbox and authoritative contracts. Do not run Git commands that change the index or commit: the worktree's shared Git metadata is outside your writable sandbox. When the scoped edits are ready, send the manager a completion message with evidence; the Factory service will validate and record the scoped changes as a source commit.\n\nRun: {}\nSlice: {} ({})\nObjective: {}\nRepository: {}\nWorktree: {}\nRecorded base SHA: {}\nAllowed paths: {}\nDependencies: {}\nContract keys: {}\nAcceptance evidence required: {}\n\nIf scope, dependencies, or contracts are unclear, ask the manager through factory_send_message before editing.",
        assignment.run_id,
        assignment.id,
        assignment.assignment_key,
        assignment.objective,
        worktree.repo_id,
        worktree.path.display(),
        worktree.base_sha,
        assignment.allowed_paths.join(", "),
        assignment.dependency_ids.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "),
        assignment.contract_keys.join(", "),
        assignment.acceptance_evidence,
    )
}

fn ensure_dependencies_integrated_or_completed(
    connection: &Connection,
    slice_id: SliceId,
    run_id: RunId,
) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT s.status FROM slice_dependencies d JOIN slice_assignments s
          ON s.id = d.dependency_slice_id AND s.run_id = d.run_id
         WHERE d.slice_id = ?1 AND d.run_id = ?2",
    )?;
    let rows = statement.query_map(params![slice_id.to_string(), run_id.to_string()], |row| {
        row.get::<_, String>(0)
    })?;
    for status in rows {
        if status? != "integrated" {
            bail!("slice dependency must be integrated before the dependent worker starts");
        }
    }
    Ok(())
}

fn ensure_contracts_resolved(
    connection: &Connection,
    slice_id: SliceId,
    run_id: RunId,
) -> Result<()> {
    let unresolved: i64 = connection.query_row(
        "SELECT COUNT(*) FROM slice_contracts sc LEFT JOIN contract_decisions cd
           ON cd.run_id = sc.run_id AND cd.contract_key = sc.contract_key
         WHERE sc.slice_id = ?1 AND sc.run_id = ?2 AND (cd.status IS NULL OR cd.status != 'resolved')",
        params![slice_id.to_string(), run_id.to_string()],
        |row| row.get(0),
    )?;
    if unresolved > 0 {
        bail!("slice is waiting for manager contract resolution");
    }
    Ok(())
}

fn ensure_commit_is_descendant(root: &Path, base: &str, head: &str) -> Result<()> {
    let output = git_output(root, &["merge-base", "--is-ancestor", base, head])?;
    if !output.status.success() {
        bail!("worker branch no longer descends from the recorded base SHA");
    }
    Ok(())
}

fn capture_scoped_worker_changes(
    worktree: &Worktree,
    assignment: &SliceAssignment,
) -> Result<String> {
    let initial_head = git_text(&worktree.path, &["rev-parse", "HEAD"])?;
    ensure_commit_is_descendant(&worktree.path, &worktree.base_sha, &initial_head)?;
    if initial_head != worktree.base_sha {
        ensure_commit_scope(worktree, assignment, &initial_head)?;
    }

    let mut pending_paths = git_paths(&worktree.path, &["diff", "--name-only", "-z", "HEAD"])?;
    pending_paths.extend(git_paths(
        &worktree.path,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?);
    ensure_paths_within_scope(assignment, &pending_paths)?;

    if !pending_paths.is_empty() {
        let mut args = vec!["add".to_owned(), "-A".to_owned(), "--".to_owned()];
        args.extend(assignment.allowed_paths.iter().cloned());
        git_status_owned(&worktree.path, &args)?;

        let staged_paths = git_paths(
            &worktree.path,
            &["diff", "--cached", "--name-only", "-z", "HEAD"],
        )?;
        ensure_paths_within_scope(assignment, &staged_paths)?;
        if staged_paths.is_empty() {
            bail!("worker reported completion but has no staged changes to record");
        }
        git_status(
            &worktree.path,
            &[
                "commit",
                "-m",
                &format!("factory: complete {}", assignment.assignment_key),
            ],
        )?;
    }

    let head = git_text(&worktree.path, &["rev-parse", "HEAD"])?;
    if head == worktree.base_sha {
        bail!("worker did not provide changes from the recorded base SHA");
    }
    ensure_commit_scope(worktree, assignment, &head)?;
    Ok(head)
}

fn ensure_paths_within_scope(assignment: &SliceAssignment, paths: &[String]) -> Result<()> {
    for path in paths {
        let normalized = crate::worktrees::normalize_scope_path(path)?;
        if !assignment
            .allowed_paths
            .iter()
            .any(|scope| path_is_within(&normalized, scope))
        {
            bail!("worker changed path '{normalized}' outside its assigned scope");
        }
    }
    Ok(())
}

fn git_paths(root: &Path, args: &[&str]) -> Result<Vec<String>> {
    let output = git_output(root, args)?;
    if !output.status.success() {
        let safe = RedactedOutput::new(String::from_utf8_lossy(&output.stderr));
        bail!("git command failed: {}", safe.as_str());
    }
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            String::from_utf8(path.to_vec()).context("worker changed a path that is not UTF-8")
        })
        .collect()
}

fn git_status_owned(root: &Path, args: &[String]) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .context("starting Git to capture scoped worker changes")?;
    if !output.status.success() {
        let safe = RedactedOutput::new(String::from_utf8_lossy(&output.stderr));
        bail!("git command failed: {}", safe.as_str());
    }
    Ok(())
}

fn ensure_commit_scope(
    worktree: &Worktree,
    assignment: &SliceAssignment,
    head: &str,
) -> Result<()> {
    ensure_commit_is_descendant(&worktree.path, &worktree.base_sha, head)?;
    let changed = git_text(
        &worktree.path,
        &["diff", "--name-only", &worktree.base_sha, head],
    )?;
    for path in changed.lines().filter(|path| !path.is_empty()) {
        let normalized = crate::worktrees::normalize_scope_path(path)?;
        if !assignment
            .allowed_paths
            .iter()
            .any(|scope| path_is_within(&normalized, scope))
        {
            bail!("completed worker commit changed path '{normalized}' outside its assigned scope");
        }
    }
    Ok(())
}

fn path_is_within(path: &str, scope: &str) -> bool {
    path == scope
        || path
            .strip_prefix(scope)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn git_text(root: &Path, args: &[&str]) -> Result<String> {
    let output = git_output(root, args)?;
    if !output.status.success() {
        let safe = RedactedOutput::new(String::from_utf8_lossy(&output.stderr));
        bail!("git command failed: {}", safe.as_str());
    }
    String::from_utf8(output.stdout)
        .context("Git output is not UTF-8")
        .map(|output| output.trim().to_owned())
}

fn git_status(root: &Path, args: &[&str]) -> Result<()> {
    let output = git_output(root, args)?;
    if !output.status.success() {
        let safe = RedactedOutput::new(String::from_utf8_lossy(&output.stderr));
        bail!("git command failed: {}", safe.as_str());
    }
    Ok(())
}

fn git_output(root: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .context("starting Git for scheduler operation")
}

fn insert_blocker(
    transaction: &rusqlite::Transaction<'_>,
    run_id: RunId,
    slice_id: Option<SliceId>,
    kind: &str,
    detail: &str,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO scheduler_blockers(id, run_id, slice_id, kind, detail, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            Uuid::new_v4().to_string(),
            run_id.to_string(),
            slice_id.map(|id| id.to_string()),
            kind,
            RedactedOutput::new(detail).as_str(),
            now_ms()
        ],
    )?;
    Ok(())
}

fn integration_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<IntegrationRecord> {
    Ok(IntegrationRecord {
        slice_id: SliceId(parse_uuid(&row.get::<_, String>(0)?).map_err(sql_error)?),
        run_id: RunId(parse_uuid(&row.get::<_, String>(1)?).map_err(sql_error)?),
        source_commit: row.get(2)?,
        source_base_commit: row.get(3)?,
        integration_base_commit: row.get(4)?,
        destination_commit: row.get(5)?,
        state: row.get(6)?,
        integrated_at_ms: row.get(7)?,
    })
}

fn blocker_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SchedulerBlocker> {
    let id = parse_uuid(&row.get::<_, String>(0)?).map_err(sql_error)?;
    let run_id = RunId(parse_uuid(&row.get::<_, String>(1)?).map_err(sql_error)?);
    let slice: Option<String> = row.get(2)?;
    Ok(SchedulerBlocker {
        id,
        run_id,
        slice_id: slice
            .map(|id| parse_uuid(&id).map(SliceId).map_err(sql_error))
            .transpose()?,
        kind: row.get(3)?,
        detail: row.get(4)?,
        created_at_ms: row.get(5)?,
        resolved_at_ms: row.get(6)?,
    })
}

fn parse_uuid(value: &str) -> std::result::Result<Uuid, uuid::Error> {
    Uuid::parse_str(value)
}

fn parse_run_id(value: &str) -> Result<RunId> {
    Ok(RunId(parse_uuid(value)?))
}

fn parse_slice_id(value: &str) -> Result<SliceId> {
    Ok(SliceId(parse_uuid(value)?))
}

fn sql_error(error: uuid::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

fn exit_text(exit: WorkerExit) -> &'static str {
    match exit {
        WorkerExit::Completed => "completed",
        WorkerExit::Interrupted => "interrupted",
        WorkerExit::Failed => "failed",
    }
}

fn ensure_active_run(connection: &Connection, run_id: RunId) -> Result<()> {
    let active: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM worktrees WHERE run_id = ?1 AND role_key = 'integration' AND state = 'active')",
        [run_id.to_string()],
        |row| row.get(0),
    )?;
    if !active {
        bail!("run has no active integration worktree");
    }
    Ok(())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
