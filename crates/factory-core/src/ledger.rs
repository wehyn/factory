use crate::model::{
    Event, EventKind, FactorySnapshot, ManagerChatMessage, ManagerChatRole, RedactedOutput, RepoId,
    RunId, RunLink, RunRecord, SequencedEvent, SessionId, SessionProcessState, SessionSnapshot,
};
use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc, Mutex,
    },
    time::Duration,
};
use uuid::Uuid;

const MAX_SNAPSHOT_OUTPUTS: usize = 40;

#[derive(Clone)]
pub struct Ledger {
    connection: Arc<Mutex<Connection>>,
    subscribers: Arc<Mutex<Vec<Sender<SequencedEvent>>>>,
    database_path: Arc<PathBuf>,
}

impl Ledger {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)
            .with_context(|| format!("opening event ledger at {}", path.display()))?;
        connection.busy_timeout(Duration::from_secs(60))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS events (
                sequence INTEGER PRIMARY KEY,
                event_id TEXT NOT NULL UNIQUE,
                session_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS manager_chat_messages (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                message_id TEXT NOT NULL UNIQUE,
                session_id TEXT NOT NULL,
                run_id TEXT,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS repositories (
                id TEXT PRIMARY KEY,
                canonical_root TEXT NOT NULL UNIQUE,
                remote_url TEXT,
                default_branch TEXT NOT NULL,
                registered_at_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS factory_runs (
                id TEXT PRIMARY KEY,
                repo_id TEXT NOT NULL REFERENCES repositories(id),
                title TEXT NOT NULL,
                base_sha TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS run_links (
                run_id TEXT NOT NULL REFERENCES factory_runs(id),
                linked_run_id TEXT NOT NULL REFERENCES factory_runs(id),
                created_at_ms INTEGER NOT NULL,
                PRIMARY KEY(run_id, linked_run_id),
                CHECK(run_id < linked_run_id)
            );
            CREATE TABLE IF NOT EXISTS worktrees (
                id TEXT PRIMARY KEY,
                repo_id TEXT NOT NULL REFERENCES repositories(id),
                run_id TEXT NOT NULL,
                role_kind TEXT NOT NULL,
                role_key TEXT NOT NULL,
                agent_id TEXT,
                base_sha TEXT NOT NULL,
                branch_name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                state TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                UNIQUE(run_id, role_key),
                UNIQUE(repo_id, branch_name)
            );
            CREATE INDEX IF NOT EXISTS worktrees_by_run ON worktrees(run_id);
            CREATE TABLE IF NOT EXISTS file_reservations (
                run_id TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                path TEXT NOT NULL,
                PRIMARY KEY(run_id, agent_id, path)
            );
            CREATE INDEX IF NOT EXISTS file_reservations_by_run ON file_reservations(run_id);
            CREATE TABLE IF NOT EXISTS recovery_issues (
                id TEXT PRIMARY KEY,
                worktree_id TEXT NOT NULL REFERENCES worktrees(id),
                kind TEXT NOT NULL,
                detail TEXT NOT NULL,
                recorded_at_ms INTEGER NOT NULL,
                resolved_at_ms INTEGER
            );
            CREATE INDEX IF NOT EXISTS recovery_issues_by_worktree
                ON recovery_issues(worktree_id, resolved_at_ms);
            CREATE TABLE IF NOT EXISTS agent_messages (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                from_kind TEXT NOT NULL,
                from_agent_id TEXT,
                to_kind TEXT NOT NULL,
                to_agent_id TEXT,
                kind TEXT NOT NULL,
                body TEXT NOT NULL,
                contract_key TEXT,
                contract_version INTEGER,
                created_at_ms INTEGER NOT NULL,
                acknowledged_at_ms INTEGER
            );
            CREATE INDEX IF NOT EXISTS agent_messages_inbox
                ON agent_messages(run_id, to_kind, to_agent_id, acknowledged_at_ms, created_at_ms);
            CREATE TABLE IF NOT EXISTS slice_assignments (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                assignment_key TEXT NOT NULL,
                objective TEXT NOT NULL,
                acceptance_evidence TEXT NOT NULL,
                allowed_paths_json TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                worktree_id TEXT REFERENCES worktrees(id),
                status TEXT NOT NULL,
                blocked_reason TEXT,
                created_at_ms INTEGER NOT NULL,
                UNIQUE(id, run_id),
                UNIQUE(run_id, assignment_key),
                UNIQUE(run_id, agent_id)
            );
            CREATE INDEX IF NOT EXISTS slice_assignments_by_run
                ON slice_assignments(run_id, created_at_ms);
            CREATE TABLE IF NOT EXISTS slice_dependencies (
                slice_id TEXT NOT NULL,
                run_id TEXT NOT NULL,
                dependency_slice_id TEXT NOT NULL,
                PRIMARY KEY(slice_id, dependency_slice_id),
                FOREIGN KEY(slice_id, run_id) REFERENCES slice_assignments(id, run_id),
                FOREIGN KEY(dependency_slice_id, run_id) REFERENCES slice_assignments(id, run_id)
            );
            CREATE TABLE IF NOT EXISTS slice_contracts (
                slice_id TEXT NOT NULL,
                run_id TEXT NOT NULL,
                contract_key TEXT NOT NULL,
                PRIMARY KEY(slice_id, contract_key),
                FOREIGN KEY(slice_id, run_id) REFERENCES slice_assignments(id, run_id)
            );
            CREATE TABLE IF NOT EXISTS contract_decisions (
                run_id TEXT NOT NULL,
                contract_key TEXT NOT NULL,
                version INTEGER NOT NULL,
                body TEXT NOT NULL,
                status TEXT NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY(run_id, contract_key)
            );
            CREATE TABLE IF NOT EXISTS contract_versions (
                run_id TEXT NOT NULL,
                contract_key TEXT NOT NULL,
                version INTEGER NOT NULL,
                body TEXT NOT NULL,
                status TEXT NOT NULL,
                source_message_id TEXT,
                created_at_ms INTEGER NOT NULL,
                PRIMARY KEY(run_id, contract_key, version)
            );
            CREATE TABLE IF NOT EXISTS mcp_tool_invocations (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                tool_name TEXT NOT NULL,
                principal_kind TEXT NOT NULL,
                agent_id TEXT,
                invoked_at_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS slice_attempts (
                id TEXT PRIMARY KEY,
                slice_id TEXT NOT NULL,
                run_id TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                attempt_number INTEGER NOT NULL,
                session_id TEXT NOT NULL UNIQUE,
                state TEXT NOT NULL,
                started_at_ms INTEGER NOT NULL,
                finished_at_ms INTEGER,
                detail TEXT,
                UNIQUE(slice_id, attempt_number)
            );
            CREATE INDEX IF NOT EXISTS slice_attempts_by_agent
                ON slice_attempts(agent_id, attempt_number);
            CREATE TABLE IF NOT EXISTS slice_integrations (
                slice_id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                source_commit TEXT NOT NULL,
                source_base_commit TEXT NOT NULL,
                integration_base_commit TEXT NOT NULL,
                destination_commit TEXT,
                state TEXT NOT NULL,
                integrated_at_ms INTEGER
            );
            CREATE TABLE IF NOT EXISTS scheduler_blockers (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                slice_id TEXT,
                kind TEXT NOT NULL,
                detail TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                resolved_at_ms INTEGER
            );",
        )?;

        ensure_column(
            &connection,
            "slice_assignments",
            "attempt_count",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(&connection, "slice_assignments", "source_commit", "TEXT")?;
        ensure_column(
            &connection,
            "slice_assignments",
            "completion_evidence",
            "TEXT",
        )?;
        ensure_column(
            &connection,
            "slice_integrations",
            "source_base_commit",
            "TEXT NOT NULL DEFAULT ''",
        )?;

        let ledger = Self {
            connection: Arc::new(Mutex::new(connection)),
            subscribers: Arc::new(Mutex::new(Vec::new())),
            database_path: Arc::new(path.to_path_buf()),
        };
        ledger.migrate_legacy_manager_chat()?;
        Ok(ledger)
    }

    pub fn database_path(&self) -> &Path {
        self.database_path.as_path()
    }

    pub fn register_run_record(&self, run: &RunRecord) -> Result<()> {
        let title = run.title.trim();
        if title.is_empty() || title.chars().count() > 256 {
            return Err(anyhow!("run title must contain 1 to 256 characters"));
        }
        self.with_connection(|connection| {
            let transaction = connection.transaction()?;
            let existing = transaction
                .query_row(
                    "SELECT repo_id, title, base_sha, created_at_ms FROM factory_runs WHERE id = ?1",
                    [run.id.to_string()],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                        ))
                    },
                )
                .optional()?;
            if let Some(existing) = existing {
                if existing
                    != (
                        run.repo_id.to_string(),
                        title.to_owned(),
                        run.base_sha.clone(),
                        run.created_at_ms,
                    )
                {
                    return Err(anyhow!("run ID was reused with different run details"));
                }
                transaction.commit()?;
                return Ok(());
            }
            transaction.execute(
                "INSERT INTO factory_runs(id, repo_id, title, base_sha, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    run.id.to_string(),
                    run.repo_id.to_string(),
                    title,
                    run.base_sha,
                    run.created_at_ms,
                ],
            )?;
            transaction.commit()?;
            Ok(())
        })
    }

    pub fn list_runs(&self) -> Result<Vec<RunRecord>> {
        self.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, repo_id, title, base_sha, created_at_ms
                 FROM factory_runs ORDER BY created_at_ms DESC, id",
            )?;
            let rows = statement.query_map([], run_record_from_row)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .context("reading registered runs")
        })
    }

    pub fn link_runs(&self, left: RunId, right: RunId) -> Result<RunLink> {
        if left == right {
            return Err(anyhow!("a run cannot link to itself"));
        }
        let (run_id, linked_run_id) = if left.to_string() < right.to_string() {
            (left, right)
        } else {
            (right, left)
        };
        self.with_connection(|connection| {
            let transaction = connection.transaction()?;
            let first = transaction
                .query_row(
                    "SELECT repo_id FROM factory_runs WHERE id = ?1",
                    [run_id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| anyhow!("run {run_id} is not registered"))?;
            let second = transaction
                .query_row(
                    "SELECT repo_id FROM factory_runs WHERE id = ?1",
                    [linked_run_id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| anyhow!("run {linked_run_id} is not registered"))?;
            if first == second {
                return Err(anyhow!("linked runs must belong to different repositories"));
            }
            let created_at_ms = transaction
                .query_row(
                    "SELECT created_at_ms FROM run_links WHERE run_id = ?1 AND linked_run_id = ?2",
                    params![run_id.to_string(), linked_run_id.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            let created_at_ms = match created_at_ms {
                Some(created_at_ms) => created_at_ms,
                None => {
                    let created_at_ms = now_ms();
                    transaction.execute(
                        "INSERT INTO run_links(run_id, linked_run_id, created_at_ms)
                         VALUES (?1, ?2, ?3)",
                        params![run_id.to_string(), linked_run_id.to_string(), created_at_ms],
                    )?;
                    created_at_ms
                }
            };
            transaction.commit()?;
            Ok(RunLink {
                run_id,
                linked_run_id,
                created_at_ms,
            })
        })
    }

    pub fn list_linked_runs(&self, run_id: RunId) -> Result<Vec<RunId>> {
        self.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT CASE WHEN run_id = ?1 THEN linked_run_id ELSE run_id END AS other_run
                 FROM run_links WHERE run_id = ?1 OR linked_run_id = ?1
                 ORDER BY other_run",
            )?;
            let rows = statement.query_map([run_id.to_string()], |row| {
                let id = row.get::<_, String>(0)?;
                Uuid::parse_str(&id).map(RunId).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .context("reading linked runs")
        })
    }

    pub fn record_manager_chat_message(
        &self,
        message_id: Uuid,
        session_id: SessionId,
        run_id: Option<RunId>,
        role: ManagerChatRole,
        content: &str,
    ) -> Result<()> {
        self.record_manager_chat_message_at(message_id, session_id, run_id, role, content, now_ms())
    }

    fn record_manager_chat_message_at(
        &self,
        message_id: Uuid,
        session_id: SessionId,
        run_id: Option<RunId>,
        role: ManagerChatRole,
        content: &str,
        created_at_ms: i64,
    ) -> Result<()> {
        let role_text = manager_chat_role_text(role);
        let content = RedactedOutput::new(content).as_str().to_owned();
        self.with_connection(|connection| {
            let transaction = connection.transaction()?;
            let existing = transaction
                .query_row(
                    "SELECT session_id, run_id, role, content FROM manager_chat_messages
                     WHERE message_id = ?1",
                    [message_id.to_string()],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    },
                )
                .optional()?;
            if let Some((stored_session, stored_run, stored_role, stored_content)) = existing {
                if stored_session != session_id.to_string()
                    || stored_run != run_id.map(|id| id.to_string())
                    || stored_role != role_text
                    || stored_content != content
                {
                    return Err(anyhow!(
                        "manager chat message ID was reused with different content"
                    ));
                }
                transaction.commit()?;
                return Ok(());
            }
            transaction.execute(
                "INSERT INTO manager_chat_messages
                    (message_id, session_id, run_id, role, content, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    message_id.to_string(),
                    session_id.to_string(),
                    run_id.map(|id| id.to_string()),
                    role_text,
                    content,
                    created_at_ms,
                ],
            )?;
            transaction.commit()?;
            Ok(())
        })
    }

    fn migrate_legacy_manager_chat(&self) -> Result<()> {
        let legacy_rows = self.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT event_id, session_id, payload_json, created_at_ms FROM events
                 WHERE kind IN ('user_message', 'assistant_message') ORDER BY sequence",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .context("reading legacy manager conversation events")
        })?;

        for (event_id, session_id, payload, created_at_ms) in legacy_rows {
            let event_id = Uuid::parse_str(&event_id)?;
            let session_id = SessionId(Uuid::parse_str(&session_id)?);
            let kind: EventKind = serde_json::from_str(&payload)
                .context("decoding legacy manager conversation event")?;
            let (role, content) = match kind {
                EventKind::UserMessage { text } => (ManagerChatRole::User, text),
                EventKind::AssistantMessage { text, .. } => (ManagerChatRole::Assistant, text),
                _ => continue,
            };
            self.record_manager_chat_message_at(
                event_id,
                session_id,
                None,
                role,
                &content,
                created_at_ms,
            )?;
        }
        Ok(())
    }

    pub fn list_manager_chat(&self) -> Result<Vec<ManagerChatMessage>> {
        self.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT message_id, session_id, run_id, role, content, created_at_ms
                 FROM (
                    SELECT sequence, message_id, session_id, run_id, role, content, created_at_ms
                    FROM manager_chat_messages ORDER BY created_at_ms DESC, sequence DESC LIMIT 200
                 ) ORDER BY created_at_ms ASC, sequence ASC",
            )?;
            let rows = statement.query_map([], manager_chat_from_row)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .context("reading persistent manager chat")
        })
    }

    pub(crate) fn with_connection<T>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T>,
    ) -> Result<T> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("event ledger connection lock was poisoned"))?;
        operation(&mut connection)
    }

    pub fn subscribe(&self) -> Result<Receiver<SequencedEvent>> {
        let (sender, receiver) = mpsc::channel();
        self.subscribers
            .lock()
            .map_err(|_| anyhow!("event subscriber lock was poisoned"))?
            .push(sender);
        Ok(receiver)
    }

    pub fn append(&self, event: &Event) -> Result<i64> {
        let event_id = event.id.to_string();
        let session_id = event.session_id.to_string();
        let kind = event.kind.kind_name();
        let payload_json = serde_json::to_string(&event.kind)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("event ledger connection lock was poisoned"))?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO events (event_id, session_id, kind, payload_json, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(event_id) DO NOTHING",
            params![
                event_id,
                session_id,
                kind,
                payload_json,
                event.created_at_ms
            ],
        )?;
        let (sequence, stored_session_id, stored_kind, stored_payload, stored_created_at_ms) =
            transaction.query_row(
                "SELECT sequence, session_id, kind, payload_json, created_at_ms
             FROM events WHERE event_id = ?1",
                [event.id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )?;
        if stored_session_id != event.session_id.to_string()
            || stored_kind != event.kind.kind_name()
            || stored_payload != serde_json::to_string(&event.kind)?
            || stored_created_at_ms != event.created_at_ms
        {
            return Err(anyhow!("event ID was reused with different content"));
        }
        transaction.commit()?;

        let published = SequencedEvent {
            sequence,
            event: event.clone(),
        };
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers.retain(|subscriber| subscriber.send(published.clone()).is_ok());
        }

        Ok(sequence)
    }

    /// Marks persisted sessions as interrupted before a restarted service exposes its snapshot.
    /// Call this once at service startup, before creating or supervising any new child processes.
    pub fn recover_unfinished_sessions(&self) -> Result<Vec<SequencedEvent>> {
        let snapshot = self.snapshot()?;
        let unfinished = snapshot
            .sessions
            .into_iter()
            .filter(|session| {
                matches!(
                    session.process_state,
                    SessionProcessState::Starting | SessionProcessState::Running
                )
            })
            .map(|session| session.session_id)
            .collect::<Vec<_>>();

        unfinished
            .into_iter()
            .map(|session_id| {
                let event = Event::new(session_id, EventKind::SessionInterrupted);
                let sequence = self.append(&event)?;
                Ok(SequencedEvent { sequence, event })
            })
            .collect()
    }

    pub fn snapshot(&self) -> Result<FactorySnapshot> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("event ledger connection lock was poisoned"))?;
        let mut statement = connection.prepare(
            "SELECT sequence, session_id, kind, payload_json, created_at_ms
             FROM events ORDER BY sequence ASC",
        )?;
        let mut rows = statement.query([])?;
        let mut snapshot = FactorySnapshot {
            last_sequence: 0,
            sessions: Vec::new(),
        };
        let mut session_indexes: HashMap<SessionId, usize> = HashMap::new();

        while let Some(row) = rows.next()? {
            let sequence = row.get::<_, i64>(0)?;
            snapshot.last_sequence = sequence;
            let session_id = SessionId(
                Uuid::parse_str(&row.get::<_, String>(1)?)
                    .context("event ledger contains an invalid session ID")?,
            );
            let kind_name = row.get::<_, String>(2)?;
            let payload_json = row.get::<_, String>(3)?;
            let created_at_ms = row.get::<_, i64>(4)?;
            let kind: EventKind = serde_json::from_str(&payload_json)
                .context("event ledger contains an invalid event payload")?;
            if kind.kind_name() != kind_name {
                return Err(anyhow!(
                    "event ledger kind column does not match its payload"
                ));
            }

            let index = match session_indexes.get(&session_id) {
                Some(index) => *index,
                None => {
                    let index = snapshot.sessions.len();
                    snapshot.sessions.push(SessionSnapshot::new(
                        session_id,
                        created_at_ms,
                        sequence,
                    ));
                    session_indexes.insert(session_id, index);
                    index
                }
            };
            let session = &mut snapshot.sessions[index];
            session.last_sequence = sequence;
            match kind {
                EventKind::SessionCreated | EventKind::ManagerSessionCreated => {}
                EventKind::SessionStarted { thread_id } => {
                    session.thread_id = Some(thread_id);
                    session.process_state = SessionProcessState::Running;
                }
                EventKind::TurnStarted { .. } => {
                    session.process_state = SessionProcessState::Running;
                }
                EventKind::Output(output) => {
                    if session.output.len() == MAX_SNAPSHOT_OUTPUTS {
                        session.output.remove(0);
                    }
                    session.output.push(output.as_str().to_owned());
                }
                EventKind::UserMessage { .. } => {}
                EventKind::AssistantMessage { text, .. } => {
                    if session.output.len() == MAX_SNAPSHOT_OUTPUTS {
                        session.output.remove(0);
                    }
                    session
                        .output
                        .push(RedactedOutput::new(text).as_str().to_owned());
                }
                EventKind::TurnCompleted => {
                    session.process_state = SessionProcessState::Completed;
                }
                EventKind::SessionInterrupted => {
                    session.process_state = SessionProcessState::Interrupted;
                }
                EventKind::SessionFailed { .. } => {
                    session.failure_count += 1;
                    session.process_state = SessionProcessState::Failed;
                }
            }
        }

        Ok(snapshot)
    }
}

fn ensure_column(
    connection: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<()> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = statement.query_map([], |row| row.get::<_, String>(1))?;
    for name in names {
        if name? == column {
            return Ok(());
        }
    }
    connection.execute_batch(&format!(
        "ALTER TABLE {table} ADD COLUMN {column} {definition}"
    ))?;
    Ok(())
}

fn run_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunRecord> {
    let id = row.get::<_, String>(0)?;
    let repo_id = row.get::<_, String>(1)?;
    let uuid_error = |column, error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    };
    Ok(RunRecord {
        id: RunId(Uuid::parse_str(&id).map_err(|error| uuid_error(0, error))?),
        repo_id: RepoId(Uuid::parse_str(&repo_id).map_err(|error| uuid_error(1, error))?),
        title: row.get(2)?,
        base_sha: row.get(3)?,
        created_at_ms: row.get(4)?,
    })
}

fn manager_chat_role_text(role: ManagerChatRole) -> &'static str {
    match role {
        ManagerChatRole::User => "user",
        ManagerChatRole::Assistant => "assistant",
    }
}

fn manager_chat_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ManagerChatMessage> {
    let id = row.get::<_, String>(0)?;
    let session_id = row.get::<_, String>(1)?;
    let run_id = row.get::<_, Option<String>>(2)?;
    let role_text = row.get::<_, String>(3)?;
    let role = match role_text.as_str() {
        "user" => ManagerChatRole::User,
        "assistant" => ManagerChatRole::Assistant,
        _ => {
            return Err(rusqlite::Error::InvalidColumnType(
                3,
                "role".to_owned(),
                rusqlite::types::Type::Text,
            ))
        }
    };
    let uuid_error = |column, error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    };
    Ok(ManagerChatMessage {
        id: Uuid::parse_str(&id).map_err(|error| uuid_error(0, error))?,
        session_id: SessionId(Uuid::parse_str(&session_id).map_err(|error| uuid_error(1, error))?),
        run_id: run_id
            .map(|id| {
                Uuid::parse_str(&id)
                    .map(RunId)
                    .map_err(|error| uuid_error(2, error))
            })
            .transpose()?,
        role,
        content: row.get(4)?,
        created_at_ms: row.get(5)?,
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
