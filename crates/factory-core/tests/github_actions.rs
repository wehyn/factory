use factory_core::{
    create_pull_request, evaluate_pull_request_gate, try_merge_pull_request,
    ExpectedPullRequestHead, GitHubCli, Ledger, PullRequestEvidence, PullRequestEvidenceInput,
    RepoId, Repository, RepositoryPolicy, RunId, RunRecord,
};
use std::{os::unix::fs::PermissionsExt, path::Path};

#[test]
fn refreshing_resolves_a_started_create_pr_action_after_observation_failure() -> anyhow::Result<()>
{
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("ledger.sqlite");
    let executable = temp.path().join("gh");
    let checks_count = temp.path().join("checks-count");
    let create_count = temp.path().join("create-count");
    let head = "a".repeat(40);
    write_fake_gh_for_create_recovery(
        &executable,
        &checks_count,
        &create_count,
        &head,
        Some("https://github.com/example/repo/pull/99"),
        99,
        "OPEN",
        true,
    )?;

    let repo_id = RepoId::new();
    let repository = Repository {
        id: repo_id,
        canonical_root: temp.path().to_path_buf(),
        remote_url: Some("https://github.com/example/repo.git".to_owned()),
        default_branch: "main".to_owned(),
        registered_at_ms: 1,
    };
    let run = RunRecord {
        id: RunId::new(),
        repo_id,
        title: "Update documentation".to_owned(),
        base_sha: "b".repeat(40),
        created_at_ms: 2,
    };
    let ledger = Ledger::open(&database)?;
    let connection = rusqlite::Connection::open(&database)?;
    connection.execute(
        "INSERT INTO repositories(id, canonical_root, remote_url, default_branch, registered_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            repository.id.to_string(),
            repository.canonical_root.to_string_lossy(),
            repository.remote_url,
            repository.default_branch,
            repository.registered_at_ms,
        ],
    )?;
    drop(connection);
    ledger.register_run_record(&run)?;
    ledger.store_pull_request_evidence(
        run.id,
        &PullRequestEvidence {
            change_summary: "Update docs".to_owned(),
            verification: vec!["git diff --check passed".to_owned()],
            ..PullRequestEvidence::default()
        },
    )?;

    let client = GitHubCli::with_executable(executable);
    let idempotency_key = format!("create-pr:{}", run.id);
    let expected_head = ExpectedPullRequestHead {
        head_branch: "factory/run-1".to_owned(),
        base_branch: "main".to_owned(),
        head_sha: head,
    };
    let policy = RepositoryPolicy::default();

    let failed_creation = create_pull_request(
        &client,
        &ledger,
        &run,
        &repository,
        &expected_head,
        &idempotency_key,
    );
    assert!(failed_creation.is_err());
    let pending_action = ledger
        .github_action(&idempotency_key)?
        .expect("create action should have been reserved");
    assert!(!pending_action.completed);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            pending_action
                .result_json
                .as_deref()
                .expect("created PR URL should be recorded")
        )?["url"],
        "https://github.com/example/repo/pull/99"
    );

    let refreshed = evaluate_pull_request_gate(
        &client,
        &ledger,
        &run,
        &repository,
        &policy,
        &expected_head,
        99,
    )?;
    assert!(
        ledger
            .github_action(&idempotency_key)?
            .expect("create action should remain recorded")
            .completed
    );

    let retried_creation = create_pull_request(
        &client,
        &ledger,
        &run,
        &repository,
        &expected_head,
        &idempotency_key,
    )?;
    assert_eq!(retried_creation, refreshed);
    assert_eq!(std::fs::read_to_string(&create_count)?, "1");
    Ok(())
}

#[test]
fn failed_create_does_not_reconcile_an_older_matching_closed_pr() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("gh");
    let checks_count = temp.path().join("checks-count");
    let create_count = temp.path().join("create-count");
    let head = "a".repeat(40);
    write_fake_gh_for_create_recovery(
        &executable,
        &checks_count,
        &create_count,
        &head,
        None,
        99,
        "CLOSED",
        false,
    )?;
    let (ledger, repository, run) = registered_run_fixture(temp.path())?;

    let client = GitHubCli::with_executable(executable);
    let idempotency_key = format!("create-pr:{}", run.id);
    let expected_head = ExpectedPullRequestHead {
        head_branch: "factory/run-1".to_owned(),
        base_branch: "main".to_owned(),
        head_sha: head,
    };
    assert!(create_pull_request(
        &client,
        &ledger,
        &run,
        &repository,
        &expected_head,
        &idempotency_key,
    )
    .is_err());

    let old_pr = evaluate_pull_request_gate(
        &client,
        &ledger,
        &run,
        &repository,
        &RepositoryPolicy::default(),
        &expected_head,
        99,
    )?;
    assert_eq!(old_pr.state.status, factory_core::PullRequestStatus::Closed);
    let action = ledger
        .github_action(&idempotency_key)?
        .expect("failed create action should remain recorded");
    assert!(
        !action.completed,
        "an older closed PR is not the create receipt"
    );
    assert!(
        action.result_json.is_none(),
        "failed command returned no PR identity"
    );
    assert_eq!(std::fs::read_to_string(&create_count)?, "1");
    assert!(create_pull_request(
        &client,
        &ledger,
        &run,
        &repository,
        &expected_head,
        &idempotency_key,
    )
    .is_err());
    assert_eq!(std::fs::read_to_string(create_count)?, "1");
    Ok(())
}

#[test]
fn refresh_does_not_complete_create_for_a_different_matching_pr_url() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("gh");
    let checks_count = temp.path().join("checks-count");
    let create_count = temp.path().join("create-count");
    let head = "a".repeat(40);
    write_fake_gh_for_create_recovery(
        &executable,
        &checks_count,
        &create_count,
        &head,
        Some("https://github.com/example/repo/pull/100"),
        99,
        "OPEN",
        true,
    )?;
    let (ledger, repository, run) = registered_run_fixture(temp.path())?;
    let client = GitHubCli::with_executable(executable);
    let idempotency_key = format!("create-pr:{}", run.id);
    let expected_head = ExpectedPullRequestHead {
        head_branch: "factory/run-1".to_owned(),
        base_branch: "main".to_owned(),
        head_sha: head,
    };

    assert!(create_pull_request(
        &client,
        &ledger,
        &run,
        &repository,
        &expected_head,
        &idempotency_key,
    )
    .is_err());
    let pending = ledger
        .github_action(&idempotency_key)?
        .expect("create action should remain recorded");
    assert!(!pending.completed);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(pending.result_json.as_deref().unwrap())?["url"],
        "https://github.com/example/repo/pull/100"
    );

    let older_pr = evaluate_pull_request_gate(
        &client,
        &ledger,
        &run,
        &repository,
        &RepositoryPolicy::default(),
        &expected_head,
        99,
    )?;
    assert_eq!(older_pr.state.status, factory_core::PullRequestStatus::Open);
    let pending = ledger.github_action(&idempotency_key)?.unwrap();
    assert!(
        !pending.completed,
        "another PR URL is not the create receipt"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(pending.result_json.as_deref().unwrap())?["url"],
        "https://github.com/example/repo/pull/100"
    );
    assert!(create_pull_request(
        &client,
        &ledger,
        &run,
        &repository,
        &expected_head,
        &idempotency_key,
    )
    .is_err());
    assert_eq!(std::fs::read_to_string(create_count)?, "1");
    Ok(())
}

#[test]
fn mismatched_pr_does_not_block_intended_creation_and_actions_remain_idempotent(
) -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("ledger.sqlite");
    let executable = temp.path().join("gh");
    let log = temp.path().join("gh-actions");
    let merged = temp.path().join("merged");
    let merge_args = temp.path().join("merge-args");
    let body_path = temp.path().join("pr-body");
    let head = "a".repeat(40);
    let merge_sha = "b".repeat(40);
    std::fs::write(
        temp.path().join(".agentic-factory.json"),
        r#"{"production":{"environment_id":"test","identity_url":"https://production.example.com/build.json","identity_json_field":"commit_sha","smoke_url":"https://production.example.com/health","timeout_seconds":10}}"#,
    )?;
    write_fake_gh(
        &executable,
        &log,
        &merged,
        &merge_args,
        &body_path,
        &head,
        &merge_sha,
    )?;

    let repo_id = RepoId::new();
    let repository = Repository {
        id: repo_id,
        canonical_root: temp.path().to_path_buf(),
        remote_url: Some("https://github.com/example/repo.git".to_owned()),
        default_branch: "main".to_owned(),
        registered_at_ms: 1,
    };
    let run = RunRecord {
        id: RunId::new(),
        repo_id,
        title: "Update documentation".to_owned(),
        base_sha: "c".repeat(40),
        created_at_ms: 2,
    };
    let ledger = Ledger::open(&database)?;
    let connection = rusqlite::Connection::open(&database)?;
    connection.execute(
        "INSERT INTO repositories(id, canonical_root, remote_url, default_branch, registered_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            repository.id.to_string(),
            repository.canonical_root.to_string_lossy(),
            repository.remote_url,
            repository.default_branch,
            repository.registered_at_ms,
        ],
    )?;
    drop(connection);
    ledger.register_run_record(&run)?;
    let evidence_input: PullRequestEvidenceInput = serde_json::from_value(serde_json::json!({
        "change_summary": "Update docs for the new flow",
        "verification": ["cargo test -p factory-core: passed"],
        "independent_review": ["Reviewer approved the current commit"],
        "decisions": ["Keep automatic merge disabled by default"],
        "limitations": ["No live production observation"],
        "worktree_commits": ["caller-supplied fabricated provenance"]
    }))?;
    let mut evidence = PullRequestEvidence::from(evidence_input);
    evidence
        .worktree_commits
        .push(format!("integration branch commit {head}"));
    ledger.store_pull_request_evidence(run.id, &evidence)?;

    let client = GitHubCli::with_executable(executable);
    let create_key = format!("create:{}", run.id);
    let expected_head = ExpectedPullRequestHead {
        head_branch: "factory/run-1".to_owned(),
        base_branch: "main".to_owned(),
        head_sha: head.clone(),
    };
    let policy = RepositoryPolicy {
        auto_merge_enabled: true,
        required_checks: vec!["CI".to_owned()],
        auto_merge_paths: vec!["docs/".to_owned()],
        max_auto_merge_changed_lines: 40,
    };
    let wrong_number = evaluate_pull_request_gate(
        &client,
        &ledger,
        &run,
        &repository,
        &policy,
        &expected_head,
        98,
    );
    assert!(wrong_number.is_err(), "an unrelated PR cannot be tracked");
    assert!(
        ledger.latest_pull_request(run.id)?.is_none(),
        "a mismatched PR must not occupy the run's tracked PR slot"
    );

    let created = create_pull_request(
        &client,
        &ledger,
        &run,
        &repository,
        &expected_head,
        &create_key,
    )?;
    let created_again = create_pull_request(
        &client,
        &ledger,
        &run,
        &repository,
        &expected_head,
        &create_key,
    )?;
    assert_eq!(created, created_again);
    let body = std::fs::read_to_string(body_path)?;
    assert!(body.contains("cargo test -p factory-core: passed"));
    assert!(body.contains("Reviewer approved the current commit"));
    assert!(!body.contains("caller-supplied fabricated provenance"));
    assert!(body.contains(&format!("integration branch commit {head}")));

    let merge_key = format!("merge:{}:99", run.id);
    let mut wrong_head = expected_head.clone();
    wrong_head.head_branch = "factory/unrelated-run".to_owned();
    let unrelated = try_merge_pull_request(
        &client,
        &ledger,
        &run,
        &repository,
        99,
        &wrong_head,
        &policy,
        &merge_key,
    )?;
    assert!(!unrelated.merged);
    assert!(matches!(
        unrelated.decision,
        factory_core::MergeDecision::Block { .. }
    ));

    let mut wrong_base = expected_head.clone();
    wrong_base.base_branch = "release".to_owned();
    let unrelated = try_merge_pull_request(
        &client,
        &ledger,
        &run,
        &repository,
        99,
        &wrong_base,
        &policy,
        &merge_key,
    )?;
    assert!(!unrelated.merged);

    let mut wrong_sha = expected_head.clone();
    wrong_sha.head_sha = "d".repeat(40);
    let unrelated = try_merge_pull_request(
        &client,
        &ledger,
        &run,
        &repository,
        99,
        &wrong_sha,
        &policy,
        &merge_key,
    )?;
    assert!(!unrelated.merged);

    let result = try_merge_pull_request(
        &client,
        &ledger,
        &run,
        &repository,
        99,
        &expected_head,
        &policy,
        &merge_key,
    )?;
    assert!(result.merged);
    let result_again = try_merge_pull_request(
        &client,
        &ledger,
        &run,
        &repository,
        99,
        &expected_head,
        &policy,
        &merge_key,
    )?;
    assert_eq!(result, result_again);
    let actions = std::fs::read_to_string(log)?;
    assert_eq!(actions.lines().filter(|line| *line == "create").count(), 1);
    assert_eq!(actions.lines().filter(|line| *line == "merge").count(), 1);
    let args = std::fs::read_to_string(merge_args)?;
    assert!(args.contains("--match-head-commit"));
    assert!(args.contains(&head));
    Ok(())
}

fn write_fake_gh(
    path: &Path,
    log: &Path,
    merged: &Path,
    merge_args: &Path,
    body_path: &Path,
    head: &str,
    merge_sha: &str,
) -> anyhow::Result<()> {
    let script = r#"#!/bin/sh
set -eu
LOG='LOG_PATH'
MERGED='MERGED_PATH'
MERGE_ARGS='MERGE_ARGS_PATH'
BODY_PATH='BODY_PATH_VALUE'
case "$4" in
  create)
    printf 'create\n' >> "$LOG"
    while [ "$#" -gt 0 ]; do
      if [ "$1" = "--body" ]; then shift; printf '%s' "$1" > "$BODY_PATH"; break; fi
      shift
    done
    printf '%s' 'https://github.com/example/repo/pull/99'
    ;;
  view)
    printf 'view\n' >> "$LOG"
    if [ "$5" = "98" ]; then
      printf '%s' '{"number":98,"url":"https://github.com/example/repo/pull/98","title":"Other run","state":"OPEN","isDraft":false,"headRefName":"factory/unrelated-run","baseRefName":"main","headRefOid":"OTHER_SHA","baseRefOid":"cccccccccccccccccccccccccccccccccccccccc","author":{"login":"waynejgarcia"},"latestReviews":[],"mergeCommit":null}'
    elif [ -f "$MERGED" ]; then
      printf '%s' '{"number":99,"url":"https://github.com/example/repo/pull/99","title":"Fixture","state":"MERGED","isDraft":false,"headRefName":"factory/run-1","baseRefName":"main","headRefOid":"HEAD_SHA","baseRefOid":"cccccccccccccccccccccccccccccccccccccccc","author":{"login":"waynejgarcia"},"latestReviews":[{"author":{"login":"reviewer"},"state":"APPROVED","commit":{"oid":"HEAD_SHA"}}],"mergeCommit":{"oid":"MERGE_SHA"}}'
    else
      printf '%s' '{"number":99,"url":"https://github.com/example/repo/pull/99","title":"Fixture","state":"OPEN","isDraft":false,"headRefName":"factory/run-1","baseRefName":"main","headRefOid":"HEAD_SHA","baseRefOid":"cccccccccccccccccccccccccccccccccccccccc","author":{"login":"waynejgarcia"},"latestReviews":[{"author":{"login":"reviewer"},"state":"APPROVED","commit":{"oid":"HEAD_SHA"}}],"mergeCommit":null}'
    fi
    ;;
  checks)
    printf 'checks\n' >> "$LOG"
    printf '%s' '[{"name":"CI","state":"SUCCESS","bucket":"pass"}]'
    ;;
  diff)
    printf 'diff\n' >> "$LOG"
    printf '%b' 'diff --git a/docs/guide.md b/docs/guide.md\n@@ -1 +1 @@\n-before\n+after\n'
    ;;
  merge)
    printf 'merge\n' >> "$LOG"
    printf '%s\n' "$@" > "$MERGE_ARGS"
    has_head=0
    while [ "$#" -gt 1 ]; do
      if [ "$1" = "--match-head-commit" ] && [ "$2" = "HEAD_SHA" ]; then has_head=1; fi
      shift
    done
    [ "$has_head" = "1" ] || exit 7
    touch "$MERGED"
    ;;
  *) exit 8 ;;
esac
"#
        .replace("LOG_PATH", &log.display().to_string())
        .replace("MERGED_PATH", &merged.display().to_string())
        .replace("MERGE_ARGS_PATH", &merge_args.display().to_string())
        .replace("BODY_PATH_VALUE", &body_path.display().to_string())
        .replace("HEAD_SHA", head)
        .replace("MERGE_SHA", merge_sha);
    std::fs::write(path, script)?;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

fn write_fake_gh_for_create_recovery(
    path: &Path,
    checks_count: &Path,
    create_count: &Path,
    head: &str,
    create_url: Option<&str>,
    observed_number: u64,
    pr_state: &str,
    fail_first_check: bool,
) -> anyhow::Result<()> {
    let script = r##"#!/bin/sh
set -eu
CHECKS_COUNT='CHECKS_COUNT_PATH'
CREATE_COUNT='CREATE_COUNT_PATH'
case "$4" in
  create)
    count=0
    if [ -f "$CREATE_COUNT" ]; then count=$(cat "$CREATE_COUNT"); fi
    count=$((count + 1))
    printf '%s' "$count" > "$CREATE_COUNT"
    if [ 'CREATE_FAIL' = 'yes' ]; then exit 9; fi
    printf '%s' 'CREATE_URL'
    ;;
  view)
    printf '%s' '{"number":OBSERVED_NUMBER,"url":"https://github.com/example/repo/pull/OBSERVED_NUMBER","title":"Fixture","state":"PR_STATE","isDraft":false,"headRefName":"factory/run-1","baseRefName":"main","headRefOid":"HEAD_SHA","baseRefOid":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","author":{"login":"wehyn"},"latestReviews":[],"mergeCommit":null}'
  ;;
  checks)
    count=0
    if [ -f "$CHECKS_COUNT" ]; then count=$(cat "$CHECKS_COUNT"); fi
    count=$((count + 1))
    printf '%s' "$count" > "$CHECKS_COUNT"
    if [ 'FAIL_FIRST_CHECK' = 'yes' ] && [ "$count" = "1" ]; then exit 8; fi
    printf '%s' '[{"name":"verify","state":"SUCCESS","bucket":"pass"}]'
    ;;
  diff)
    printf '%s' 'diff --git a/docs/guide.md b/docs/guide.md'
    ;;
  *) exit 2 ;;
esac
"##
    .replace("CHECKS_COUNT_PATH", &checks_count.display().to_string())
    .replace("CREATE_COUNT_PATH", &create_count.display().to_string())
    .replace("CREATE_FAIL", if create_url.is_some() { "no" } else { "yes" })
    .replace(
        "CREATE_URL",
        create_url.unwrap_or("https://github.com/example/repo/pull/0"),
    )
    .replace("OBSERVED_NUMBER", &observed_number.to_string())
    .replace("PR_STATE", pr_state)
    .replace(
        "FAIL_FIRST_CHECK",
        if fail_first_check { "yes" } else { "no" },
    )
    .replace("HEAD_SHA", head);
    std::fs::write(path, script)?;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

fn registered_run_fixture(root: &Path) -> anyhow::Result<(Ledger, Repository, RunRecord)> {
    let database = root.join("ledger.sqlite");
    let repo_id = RepoId::new();
    let repository = Repository {
        id: repo_id,
        canonical_root: root.to_path_buf(),
        remote_url: Some("https://github.com/example/repo.git".to_owned()),
        default_branch: "main".to_owned(),
        registered_at_ms: 1,
    };
    let run = RunRecord {
        id: RunId::new(),
        repo_id,
        title: "Update documentation".to_owned(),
        base_sha: "b".repeat(40),
        created_at_ms: 2,
    };
    let ledger = Ledger::open(&database)?;
    let connection = rusqlite::Connection::open(&database)?;
    connection.execute(
        "INSERT INTO repositories(id, canonical_root, remote_url, default_branch, registered_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            repository.id.to_string(),
            repository.canonical_root.to_string_lossy(),
            repository.remote_url,
            repository.default_branch,
            repository.registered_at_ms,
        ],
    )?;
    drop(connection);
    ledger.register_run_record(&run)?;
    Ok((ledger, repository, run))
}
