use factory_core::{
    Ledger, MergeDecision, PullRequestRecord, PullRequestState, PullRequestStatus, RepoId, RunId,
    RunRecord,
};

#[test]
fn pull_request_observations_and_action_receipts_survive_ledger_reopen() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("ledger.sqlite");
    let repo_id = RepoId::new();
    let run = RunRecord {
        id: RunId::new(),
        repo_id,
        title: "Document the release workflow".to_owned(),
        base_sha: "a".repeat(40),
        created_at_ms: 10,
    };
    let pull_request = PullRequestRecord {
        run_id: run.id,
        repo_id,
        state: PullRequestState {
            number: 14,
            url: "https://github.com/example/repo/pull/14".to_owned(),
            title: run.title.clone(),
            status: PullRequestStatus::Open,
            is_draft: false,
            head_branch: "factory/run-14".to_owned(),
            base_branch: "main".to_owned(),
            head_sha: "b".repeat(40),
            base_sha: run.base_sha.clone(),
            author_login: "waynejgarcia".to_owned(),
            reviews: Vec::new(),
            checks: Vec::new(),
            merged_sha: None,
            observed_at_ms: 20,
        },
        gate: Some(MergeDecision::WaitForReview {
            reason: "requires independent review".to_owned(),
        }),
    };
    let action_result = serde_json::to_string(&pull_request)?;

    {
        let ledger = Ledger::open(&path)?;
        let connection = rusqlite::Connection::open(&path)?;
        connection.execute(
            "INSERT INTO repositories(id, canonical_root, default_branch, registered_at_ms)
             VALUES (?1, '/tmp/github-fixture', 'main', 1)",
            [repo_id.to_string()],
        )?;
        drop(connection);
        ledger.register_run_record(&run)?;
        ledger.store_pull_request(&pull_request)?;
        assert!(ledger.begin_github_action("create:run-14", run.id, "create_pull_request")?);
        ledger.complete_github_action("create:run-14", &action_result)?;
        assert!(!ledger.begin_github_action("create:run-14", run.id, "create_pull_request")?);
        assert!(ledger
            .begin_github_action("create:run-14", run.id, "merge_pull_request")
            .is_err());
    }

    let reopened = Ledger::open(&path)?;
    assert_eq!(reopened.latest_pull_request(run.id)?, Some(pull_request));
    let action = reopened
        .github_action("create:run-14")?
        .expect("the reserved action remains in the ledger");
    assert_eq!(action.run_id, run.id);
    assert_eq!(action.action_kind, "create_pull_request");
    assert!(action.completed);
    assert_eq!(action.result_json.as_deref(), Some(action_result.as_str()));
    assert!(action.created_at_ms > 0);
    Ok(())
}
