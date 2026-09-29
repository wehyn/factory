use factory_core::production::{
    evaluate_production, load_production_policy, ProductionInput, ProductionObserver,
    ProductionPolicy, ProductionStatus, SmokeCheckState,
};
use factory_core::{Ledger, RepoId, RunId, RunRecord};
use std::{os::unix::fs::PermissionsExt, path::Path};

#[test]
fn production_is_healthy_only_for_matching_deployment_and_passing_smoke() {
    let run_id = RunId::new();
    let observe = |expected_sha: Option<&str>, deployed_sha: Option<&str>, smoke| {
        evaluate_production(ProductionInput {
            run_id,
            expected_sha: expected_sha.map(str::to_owned),
            deployed_sha: deployed_sha.map(str::to_owned),
            smoke,
            environment_id: Some("production".to_owned()),
            detail: String::new(),
        })
    };
    let expected = "a".repeat(40);

    assert_eq!(
        observe(Some(&expected), Some(&expected), SmokeCheckState::Passed).status,
        ProductionStatus::Healthy
    );
    assert_eq!(
        observe(
            Some(&expected),
            Some(&"b".repeat(40)),
            SmokeCheckState::Passed
        )
        .status,
        ProductionStatus::WaitingForDeployment
    );
    assert_eq!(
        observe(Some(&expected), Some(&expected), SmokeCheckState::Failed).status,
        ProductionStatus::Failed
    );
    assert_eq!(
        observe(Some(&expected), Some(&expected), SmokeCheckState::Missing).status,
        ProductionStatus::Unverified
    );
    assert_eq!(
        observe(None, None, SmokeCheckState::Missing).status,
        ProductionStatus::Unverified
    );
}

#[test]
fn production_alert_is_deduplicated_acknowledged_and_resolved_across_restarts() -> anyhow::Result<()>
{
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("ledger.sqlite");
    let expected = "b".repeat(40);
    let (ledger, run) = registered_production_run(&path)?;

    let failed = evaluate_production(ProductionInput {
        run_id: run.id,
        expected_sha: Some(expected.clone()),
        deployed_sha: Some(expected.clone()),
        smoke: SmokeCheckState::Failed,
        environment_id: Some("production".to_owned()),
        detail: "Production smoke returned HTTP 503".to_owned(),
    });
    let created = ledger.record_production_observation(&failed)?;
    assert!(created.new_alert);
    let alert_id = created
        .state
        .alert
        .as_ref()
        .expect("failed smoke alerts")
        .id;

    let mut repeated = failed.clone();
    repeated.id = uuid::Uuid::new_v4();
    repeated.observed_at_ms += 1;
    assert!(!ledger.record_production_observation(&repeated)?.new_alert);
    ledger.acknowledge_production_alert(run.id, alert_id)?;

    drop(ledger);
    let reopened = Ledger::open(&path)?;
    let persisted = reopened
        .latest_production_state(run.id)?
        .expect("watch state is durable");
    let alert = persisted.alert.expect("alert survives reopen");
    assert_eq!(alert.id, alert_id);
    assert!(alert.acknowledged_at_ms.is_some());
    assert_eq!(
        persisted.observation.expect("observation survives").status,
        ProductionStatus::Failed
    );

    let mut healthy = evaluate_production(ProductionInput {
        run_id: run.id,
        expected_sha: Some(expected.clone()),
        deployed_sha: Some(expected.clone()),
        smoke: SmokeCheckState::Passed,
        environment_id: Some("production".to_owned()),
        detail: "Deployment commit matches and smoke passed".to_owned(),
    });
    healthy.observed_at_ms = repeated.observed_at_ms + 1;
    let recovered = reopened.record_production_observation(&healthy)?;
    assert_eq!(
        recovered
            .state
            .observation
            .expect("healthy state persists")
            .status,
        ProductionStatus::Healthy
    );
    assert!(recovered
        .state
        .alert
        .expect("alert history remains visible")
        .resolved_at_ms
        .is_some());

    let mut missing = evaluate_production(ProductionInput {
        run_id: run.id,
        expected_sha: Some(expected),
        deployed_sha: None,
        smoke: SmokeCheckState::Missing,
        environment_id: None,
        detail: "Deployment identity evidence is missing".to_owned(),
    });
    missing.observed_at_ms = healthy.observed_at_ms + 1;
    let unverified = reopened.record_production_observation(&missing)?;
    assert!(unverified.new_alert);
    assert_eq!(
        unverified
            .state
            .observation
            .expect("missing evidence persists")
            .status,
        ProductionStatus::Unverified
    );
    assert_eq!(
        unverified
            .state
            .alert
            .expect("missing evidence alerts")
            .status,
        ProductionStatus::Unverified
    );
    Ok(())
}

#[test]
fn mismatched_deployment_persists_waiting_alert_across_restart() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("ledger.sqlite");
    let (ledger, run) = registered_production_run(&path)?;
    let expected = "b".repeat(40);
    let deployed = "a".repeat(40);
    let mismatch = evaluate_production(ProductionInput {
        run_id: run.id,
        expected_sha: Some(expected.clone()),
        deployed_sha: Some(deployed.clone()),
        smoke: SmokeCheckState::Passed,
        environment_id: Some("staging-release-check".to_owned()),
        detail: "Deployment is still serving the previous commit".to_owned(),
    });

    assert_eq!(mismatch.status, ProductionStatus::WaitingForDeployment);
    let recorded = ledger.record_production_observation(&mismatch)?;
    assert!(recorded.new_alert);
    let alert_id = recorded.state.alert.as_ref().unwrap().id;
    assert_eq!(
        recorded.state.alert.as_ref().unwrap().status,
        ProductionStatus::WaitingForDeployment
    );

    let mut repeated = mismatch.clone();
    repeated.id = uuid::Uuid::new_v4();
    repeated.observed_at_ms += 1;
    let duplicate = ledger.record_production_observation(&repeated)?;
    assert!(
        !duplicate.new_alert,
        "the same SHA mismatch should reuse its alert"
    );
    assert_eq!(duplicate.state.alert.unwrap().id, alert_id);

    drop(ledger);
    let reopened = Ledger::open(&path)?;
    let persisted = reopened
        .latest_production_state(run.id)?
        .expect("mismatch state survives reopening the ledger");
    let observation = persisted.observation.expect("observation is persisted");
    assert_eq!(observation.status, ProductionStatus::WaitingForDeployment);
    assert_eq!(observation.expected_sha.as_deref(), Some(expected.as_str()));
    assert_eq!(observation.deployed_sha.as_deref(), Some(deployed.as_str()));
    let alert = persisted.alert.expect("mismatch alert is persisted");
    assert_eq!(alert.id, alert_id);
    assert_eq!(alert.status, ProductionStatus::WaitingForDeployment);
    assert_eq!(alert.expected_sha.as_deref(), Some(expected.as_str()));
    Ok(())
}

#[test]
fn repository_production_config_rejects_http_and_embedded_credentials() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    std::fs::write(
        root.path().join(".agentic-factory.json"),
        r#"{"production":{"identity_url":"http://127.0.0.1/build","smoke_url":"https://example.com/health"}}"#,
    )?;
    assert!(load_production_policy(root.path()).is_err());

    std::fs::write(
        root.path().join(".agentic-factory.json"),
        r#"{"production":{"identity_url":"https://user:secret@example.com/build","smoke_url":"https://example.com/health"}}"#,
    )?;
    assert!(load_production_policy(root.path()).is_err());
    Ok(())
}

#[test]
fn production_monitor_shutdown_marker_survives_restart_until_gap_reporting_completes(
) -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("ledger.sqlite");
    {
        let ledger = Ledger::open(&path)?;
        ledger.mark_production_monitor_stopped(1234)?;
    }
    let reopened = Ledger::open(&path)?;
    assert_eq!(reopened.production_monitor_stopped_at()?, Some(1234));
    reopened.start_production_monitor_session(5678)?;
    assert_eq!(reopened.production_monitor_stopped_at()?, None);
    assert_eq!(reopened.production_monitor_started_at()?, Some(5678));
    Ok(())
}

#[test]
fn production_observer_makes_one_identity_and_smoke_request_without_retries() -> anyhow::Result<()>
{
    let sha = "c".repeat(40);
    let policy = ProductionPolicy {
        environment_id: Some("production".to_owned()),
        identity_url: Some("https://production.example.com/build.json".to_owned()),
        identity_json_field: "deployment.commit_sha".to_owned(),
        smoke_url: Some("https://production.example.com/health".to_owned()),
        timeout_seconds: 1,
    };

    for (http_status, expected_status) in [
        ("200", ProductionStatus::Healthy),
        ("503", ProductionStatus::Failed),
    ] {
        let temp = tempfile::tempdir()?;
        let executable = temp.path().join("curl");
        let calls = temp.path().join("calls");
        write_fake_curl(&executable, &calls, http_status)?;
        let observation = ProductionObserver::with_executable(executable).observe(
            RunId::new(),
            Some(&sha),
            &policy,
        );
        assert_eq!(observation.status, expected_status);
        assert_eq!(std::fs::read_to_string(calls)?, "2");
    }
    Ok(())
}

fn write_fake_curl(path: &Path, counter: &Path, smoke_status: &str) -> anyhow::Result<()> {
    let script = format!(
        r#"#!/bin/sh
counter='{}'
count=0
if [ -f "$counter" ]; then count=$(cat "$counter"); fi
count=$((count + 1))
printf '%s' "$count" > "$counter"
case "$*" in
  *--write-out*) printf '{}' ;;
  *) printf '{{"deployment":{{"commit_sha":"{}"}}}}' ;;
esac
"#,
        counter.display(),
        smoke_status,
        "c".repeat(40),
    );
    std::fs::write(path, script)?;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

fn registered_production_run(path: &Path) -> anyhow::Result<(Ledger, RunRecord)> {
    let repo_id = RepoId::new();
    let run = RunRecord {
        id: RunId::new(),
        repo_id,
        title: "Observe a release".to_owned(),
        base_sha: "a".repeat(40),
        created_at_ms: 1,
    };
    let ledger = Ledger::open(path)?;
    let connection = rusqlite::Connection::open(path)?;
    connection.execute(
        "INSERT INTO repositories(id, canonical_root, default_branch, registered_at_ms)
         VALUES (?1, '/tmp/production-fixture', 'main', 1)",
        [repo_id.to_string()],
    )?;
    drop(connection);
    ledger.register_run_record(&run)?;
    Ok((ledger, run))
}
