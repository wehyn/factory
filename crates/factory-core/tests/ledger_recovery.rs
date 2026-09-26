use factory_core::{Event, EventKind, Ledger, RedactedOutput, SessionId};

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
