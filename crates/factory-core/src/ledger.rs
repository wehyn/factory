use crate::model::{
    Event, EventKind, FactorySnapshot, SequencedEvent, SessionId, SessionProcessState,
    SessionSnapshot,
};
use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection};
use std::{
    collections::HashMap,
    path::Path,
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
            CREATE TABLE IF NOT EXISTS repositories (
                id TEXT PRIMARY KEY,
                canonical_root TEXT NOT NULL UNIQUE,
                remote_url TEXT,
                default_branch TEXT NOT NULL,
                registered_at_ms INTEGER NOT NULL
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
            );",
        )?;

        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            subscribers: Arc::new(Mutex::new(Vec::new())),
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
                EventKind::SessionCreated => {}
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
