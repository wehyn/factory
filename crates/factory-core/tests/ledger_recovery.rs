use factory_core::{
    Event, EventKind, Ledger, ManagerChatRole, RedactedOutput, RepoId, RunId, RunRecord, SessionId,
};

#[test]
fn reopens_ordered_session_without_duplicate_events() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("ledger.sqlite");
    let session = SessionId(uuid::Uuid::new_v4());
    let created = Event::new(session, EventKind::SessionCreated);
    let output = Event::new(session, EventKind::Output(RedactedOutput::new("hello")));

    {
        let ledger = Ledger::open(&path)?;
        assert_eq!(ledger.append(&created)?, 1);
        assert_eq!(ledger.append(&output)?, 2);
        assert_eq!(ledger.append(&output)?, 2);
    }

    let snapshot = Ledger::open(&path)?.snapshot()?;
    assert_eq!(snapshot.last_sequence, 2);
    assert_eq!(snapshot.sessions[0].output, vec!["hello"]);
    Ok(())
}

#[test]
fn redacts_credentials_before_output_is_persisted() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("ledger.sqlite");
    let raw = "Authorization: Bearer sample-secret\napi_key=another-secret\nOPENAI_API_KEY=environment-secret\nAWS_SECRET_ACCESS_KEY=cloud-secret";
    let event = Event::new(
        SessionId(uuid::Uuid::new_v4()),
        EventKind::Output(RedactedOutput::new(raw)),
    );

    let ledger = Ledger::open(&path)?;
    ledger.append(&event)?;
    let snapshot = ledger.snapshot()?;
    let mut database_bytes = std::fs::read(&path)?;
    let wal_path = std::path::PathBuf::from(format!("{}-wal", path.display()));
    if let Ok(wal_bytes) = std::fs::read(wal_path) {
        database_bytes.extend(wal_bytes);
    }

    assert!(!database_bytes
        .windows(b"sample-secret".len())
        .any(|bytes| bytes == b"sample-secret"));
    assert!(!database_bytes
        .windows(b"another-secret".len())
        .any(|bytes| bytes == b"another-secret"));
    assert!(!database_bytes
        .windows(b"environment-secret".len())
        .any(|bytes| bytes == b"environment-secret"));
    assert!(!database_bytes
        .windows(b"cloud-secret".len())
        .any(|bytes| bytes == b"cloud-secret"));
    assert!(!snapshot.sessions[0].output[0].contains("sample-secret"));
    assert!(!snapshot.sessions[0].output[0].contains("another-secret"));
    assert!(!snapshot.sessions[0].output[0].contains("environment-secret"));
    assert!(!snapshot.sessions[0].output[0].contains("cloud-secret"));
    Ok(())
}

#[test]
fn snapshot_sequence_matches_the_last_projected_row_across_connections() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("ledger.sqlite");
    let writer = Ledger::open(&path)?;
    let reader = Ledger::open(&path)?;
    let session = SessionId(uuid::Uuid::new_v4());

    for _ in 0..256 {
        writer.append(&Event::new(session, EventKind::SessionCreated))?;
        let snapshot = reader.snapshot()?;
        assert_eq!(
            snapshot.last_sequence,
            snapshot
                .sessions
                .iter()
                .map(|session| session.last_sequence)
                .max()
                .unwrap_or(0)
        );
    }

    Ok(())
}

#[test]
fn restart_recovery_interrupts_sessions_without_a_supervised_child() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("ledger.sqlite");
    let session_id = SessionId(uuid::Uuid::new_v4());
    {
        let ledger = Ledger::open(&path)?;
        ledger.append(&Event::new(session_id, EventKind::SessionCreated))?;
        ledger.append(&Event::new(
            session_id,
            EventKind::SessionStarted {
                thread_id: "thread-1".to_owned(),
            },
        ))?;
    }

    let recovered = Ledger::open(&path)?;
    let recovery_events = recovered.recover_unfinished_sessions()?;
    assert_eq!(recovery_events.len(), 1);
    assert!(matches!(
        recovery_events[0].event.kind,
        EventKind::SessionInterrupted
    ));
    assert_eq!(
        recovered.snapshot()?.sessions[0].process_state,
        factory_core::SessionProcessState::Interrupted
    );
    assert!(recovered.recover_unfinished_sessions()?.is_empty());
    Ok(())
}

#[test]
fn subscribers_receive_committed_events_with_the_ledger_sequence() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("ledger.sqlite");
    let writer = Ledger::open(&path)?;
    let reader = Ledger::open(&path)?;
    let events = writer.subscribe()?;
    let event = Event::new(
        SessionId(uuid::Uuid::new_v4()),
        EventKind::Output(RedactedOutput::new("hello")),
    );

    assert_eq!(writer.append(&event)?, 1);
    let published = events.recv_timeout(std::time::Duration::from_secs(1))?;
    assert_eq!(published.sequence, 1);
    assert_eq!(published.event, event);
    assert_eq!(reader.snapshot()?.last_sequence, published.sequence);
    Ok(())
}

#[test]
fn subscribers_do_not_drop_or_reorder_events_when_a_burst_exceeds_the_queue_capacity(
) -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let ledger = Ledger::open(&temp.path().join("ledger.sqlite"))?;
    let events = ledger.subscribe()?;
    let session_id = SessionId(uuid::Uuid::new_v4());
    let writers = (0..4)
        .map(|worker| {
            let ledger = ledger.clone();
            std::thread::spawn(move || {
                for index in 0..100 {
                    ledger
                        .append(&Event::new(
                            session_id,
                            EventKind::Output(RedactedOutput::new(format!("{worker}-{index}"))),
                        ))
                        .unwrap();
                }
            })
        })
        .collect::<Vec<_>>();

    for writer in writers {
        writer.join().expect("ledger writer did not panic");
    }

    for expected_sequence in 1..=400 {
        let event = events.recv_timeout(std::time::Duration::from_secs(1))?;
        assert_eq!(event.sequence, expected_sequence);
    }
    Ok(())
}

#[test]
fn recovered_snapshot_keeps_only_the_most_recent_forty_outputs() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let ledger = Ledger::open(&temp.path().join("ledger.sqlite"))?;
    let session_id = SessionId(uuid::Uuid::new_v4());
    ledger.append(&Event::new(session_id, EventKind::SessionCreated))?;
    for index in 0..45 {
        ledger.append(&Event::new(
            session_id,
            EventKind::Output(RedactedOutput::new(format!("output-{index}"))),
        ))?;
    }

    let output = &ledger.snapshot()?.sessions[0].output;
    assert_eq!(output.len(), 40);
    assert_eq!(output.first().map(String::as_str), Some("output-5"));
    assert_eq!(output.last().map(String::as_str), Some("output-44"));
    Ok(())
}

#[test]
fn rejects_reusing_an_event_id_with_different_content() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let ledger = Ledger::open(&temp.path().join("ledger.sqlite"))?;
    let created = Event::new(SessionId(uuid::Uuid::new_v4()), EventKind::SessionCreated);
    let mut conflicting = created.clone();
    conflicting.kind = EventKind::Output(RedactedOutput::new("not stored"));

    assert_eq!(ledger.append(&created)?, 1);
    assert!(ledger.append(&conflicting).is_err());
    assert_eq!(ledger.snapshot()?.last_sequence, 1);
    Ok(())
}

#[test]
fn manager_chat_survives_reopen_and_redacts_assistant_output() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("ledger.sqlite");
    let session_id = SessionId(uuid::Uuid::new_v4());
    let run_id = RunId::new();
    let user_id = uuid::Uuid::new_v4();
    let assistant_id = uuid::Uuid::new_v4();
    {
        let ledger = Ledger::open(&path)?;
        ledger.record_manager_chat_message(
            user_id,
            session_id,
            Some(run_id),
            ManagerChatRole::User,
            "Please check this run.",
        )?;
        ledger.record_manager_chat_message(
            assistant_id,
            session_id,
            Some(run_id),
            ManagerChatRole::Assistant,
            "Authorization: Bearer manager-secret",
        )?;
    }

    let messages = Ledger::open(&path)?.list_manager_chat()?;
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, ManagerChatRole::User);
    assert_eq!(messages[0].run_id, Some(run_id));
    assert_eq!(messages[1].role, ManagerChatRole::Assistant);
    assert!(!messages[1].content.contains("manager-secret"));
    Ok(())
}

#[test]
fn reads_legacy_manager_event_kinds_and_migrates_their_chat() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("ledger.sqlite");
    let session_id = uuid::Uuid::new_v4();
    let created_id = uuid::Uuid::new_v4();
    let started_id = uuid::Uuid::new_v4();
    let user_id = uuid::Uuid::new_v4();
    let assistant_id = uuid::Uuid::new_v4();
    let connection = rusqlite::Connection::open(&path)?;
    connection.execute_batch(
        "CREATE TABLE events (
            sequence INTEGER PRIMARY KEY,
            event_id TEXT NOT NULL UNIQUE,
            session_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL
        );",
    )?;
    for (sequence, event_id, kind, payload, created_at_ms) in [
        (
            1,
            created_id,
            "manager_session_created",
            r#"{"type":"manager_session_created"}"#.to_owned(),
            1,
        ),
        (
            2,
            started_id,
            "session_started",
            r#"{"type":"session_started","data":{"thread_id":"thread-old"}}"#.to_owned(),
            2,
        ),
        (
            3,
            user_id,
            "user_message",
            r#"{"type":"user_message","data":{"text":"Keep my old request."}}"#.to_owned(),
            3,
        ),
        (
            4,
            assistant_id,
            "assistant_message",
            format!(
                r#"{{"type":"assistant_message","data":{{"item_id":"{}","text":"Authorization: Bearer legacy-secret-token"}}}}"#,
                uuid::Uuid::new_v4()
            ),
            4,
        ),
        (
            5,
            uuid::Uuid::new_v4(),
            "turn_completed",
            r#"{"type":"turn_completed"}"#.to_owned(),
            5,
        ),
    ] {
        connection.execute(
            "INSERT INTO events(sequence, event_id, session_id, kind, payload_json, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                sequence,
                event_id.to_string(),
                session_id.to_string(),
                kind,
                payload,
                created_at_ms
            ],
        )?;
    }
    drop(connection);

    let ledger = Ledger::open(&path)?;
    let snapshot = ledger.snapshot()?;
    assert_eq!(snapshot.last_sequence, 5);
    assert_eq!(
        snapshot.sessions[0].thread_id.as_deref(),
        Some("thread-old")
    );
    assert_eq!(
        snapshot.sessions[0].process_state,
        factory_core::SessionProcessState::Completed
    );
    let chat = ledger.list_manager_chat()?;
    assert_eq!(chat.len(), 2);
    assert_eq!(chat[0].role, ManagerChatRole::User);
    assert_eq!(chat[0].content, "Keep my old request.");
    assert_eq!(chat[1].role, ManagerChatRole::Assistant);
    assert!(!chat[1].content.contains("legacy-secret-token"));

    drop(ledger);
    let reopened = Ledger::open(&path)?;
    assert_eq!(reopened.list_manager_chat()?.len(), 2);
    Ok(())
}

#[test]
fn factory_runs_and_cross_repository_links_are_persistent_and_idempotent() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("ledger.sqlite");
    let ledger = Ledger::open(&path)?;
    let first_repo = RepoId::new();
    let second_repo = RepoId::new();
    let same_repo = RepoId::new();
    let connection = rusqlite::Connection::open(&path)?;
    for (repo_id, name) in [
        (first_repo, "first"),
        (second_repo, "second"),
        (same_repo, "same"),
    ] {
        connection.execute(
            "INSERT INTO repositories(id, canonical_root, default_branch, registered_at_ms)
             VALUES (?1, ?2, 'main', 1)",
            rusqlite::params![repo_id.to_string(), format!("/tmp/{name}")],
        )?;
    }
    drop(connection);

    let first = RunRecord {
        id: RunId::new(),
        repo_id: first_repo,
        title: "First task".to_owned(),
        base_sha: "a".repeat(40),
        created_at_ms: 2,
    };
    let second = RunRecord {
        id: RunId::new(),
        repo_id: second_repo,
        title: "Second task".to_owned(),
        base_sha: "b".repeat(40),
        created_at_ms: 3,
    };
    let same_repo_run = RunRecord {
        id: RunId::new(),
        repo_id: first_repo,
        title: "Another task in the first repository".to_owned(),
        base_sha: "c".repeat(40),
        created_at_ms: 4,
    };
    for run in [&first, &second, &same_repo_run] {
        ledger.register_run_record(run)?;
    }

    let link = ledger.link_runs(first.id, second.id)?;
    assert_eq!(ledger.link_runs(second.id, first.id)?, link);
    assert_eq!(ledger.list_linked_runs(first.id)?, vec![second.id]);
    assert_eq!(ledger.list_linked_runs(second.id)?, vec![first.id]);
    assert!(ledger.link_runs(first.id, same_repo_run.id).is_err());
    assert_eq!(ledger.list_runs()?.len(), 3);

    let reopened = Ledger::open(&path)?;
    assert_eq!(reopened.list_linked_runs(first.id)?, vec![second.id]);
    assert_eq!(reopened.list_runs()?[0], same_repo_run);
    Ok(())
}
