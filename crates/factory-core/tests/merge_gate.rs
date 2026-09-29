use factory_core::policy::{
    classify_risk, load_repository_policy, merge_decision, render_pull_request_body, CheckEvidence,
    CheckState, GateInput, MergeDecision, PullRequestEvidence, RepositoryPolicy, ReviewEvidence,
    ReviewState, RiskLevel,
};

fn approved_gate() -> GateInput {
    GateInput {
        pr_open: true,
        pr_draft: false,
        expected_head_sha: "head-1".to_owned(),
        observed_head_sha: "head-1".to_owned(),
        author_login: "author".to_owned(),
        reviews: vec![ReviewEvidence {
            author_login: "reviewer".to_owned(),
            state: ReviewState::Approved,
            head_sha: "head-1".to_owned(),
            submitted_at_ms: 1,
        }],
        checks: vec![CheckEvidence {
            name: "CI".to_owned(),
            state: CheckState::Passed,
            head_sha: "head-1".to_owned(),
        }],
        required_checks: vec!["CI".to_owned()],
        risk: factory_core::policy::RiskDecision {
            level: RiskLevel::Low,
            reasons: Vec::new(),
        },
        auto_merge_enabled: true,
    }
}

#[test]
fn current_low_risk_head_with_required_checks_and_independent_review_can_merge() {
    assert_eq!(merge_decision(&approved_gate()), MergeDecision::AutoMerge);
}

#[test]
fn approval_for_an_older_commit_cannot_authorize_merge() {
    let mut input = approved_gate();
    input.reviews[0].head_sha = "older-head".to_owned();

    assert!(matches!(
        merge_decision(&input),
        MergeDecision::WaitForReview { .. }
    ));
}

#[test]
fn stale_missing_pending_and_failed_required_checks_block_merge() {
    let mut input = approved_gate();
    input.expected_head_sha = "head-2".to_owned();
    assert!(matches!(
        merge_decision(&input),
        MergeDecision::Block { .. }
    ));

    let mut input = approved_gate();
    input.checks.clear();
    assert!(matches!(
        merge_decision(&input),
        MergeDecision::Block { .. }
    ));

    let mut input = approved_gate();
    input.checks[0].state = CheckState::Pending;
    assert!(matches!(
        merge_decision(&input),
        MergeDecision::Block { .. }
    ));

    let mut input = approved_gate();
    input.checks[0].state = CheckState::Failed;
    assert!(matches!(
        merge_decision(&input),
        MergeDecision::Block { .. }
    ));

    let mut input = approved_gate();
    input.checks[0].head_sha = "older-head".to_owned();
    assert!(matches!(
        merge_decision(&input),
        MergeDecision::Block { .. }
    ));

    let mut input = approved_gate();
    input.required_checks.clear();
    assert!(matches!(
        merge_decision(&input),
        MergeDecision::Block { .. }
    ));
}

#[test]
fn self_approval_and_sensitive_changes_wait_for_wayne() {
    let mut input = approved_gate();
    input.reviews[0].author_login = "author".to_owned();
    assert!(matches!(
        merge_decision(&input),
        MergeDecision::WaitForReview { .. }
    ));

    let policy = RepositoryPolicy {
        auto_merge_enabled: true,
        required_checks: vec!["CI".to_owned()],
        auto_merge_paths: vec!["src/bugfix.rs".to_owned()],
        max_auto_merge_changed_lines: 40,
    };
    let risk = classify_risk(
        "diff --git a/src/bugfix.rs b/src/bugfix.rs\n@@ -1 +1 @@\n-old\n+require_authorization = true\n",
        &policy,
    );
    assert_eq!(risk.level, RiskLevel::HumanReview);
    input.risk = risk;
    assert!(matches!(
        merge_decision(&input),
        MergeDecision::WaitForReview { .. }
    ));
}

#[test]
fn latest_reviewer_decision_wins_and_explicitly_allowlisted_small_fixes_are_low_risk() {
    let policy = RepositoryPolicy {
        auto_merge_enabled: true,
        required_checks: vec!["CI".to_owned()],
        auto_merge_paths: vec!["src/bugfix.rs".to_owned()],
        max_auto_merge_changed_lines: 40,
    };
    let diff = "diff --git a/src/bugfix.rs b/src/bugfix.rs\n@@ -1 +1 @@\n-old\n+fixed\n";
    assert_eq!(classify_risk(diff, &policy).level, RiskLevel::Low);

    let mut input = approved_gate();
    input.reviews.extend([
        ReviewEvidence {
            author_login: "reviewer".to_owned(),
            state: ReviewState::Approved,
            head_sha: "head-1".to_owned(),
            submitted_at_ms: 1,
        },
        ReviewEvidence {
            author_login: "reviewer".to_owned(),
            state: ReviewState::ChangesRequested,
            head_sha: "head-1".to_owned(),
            submitted_at_ms: 2,
        },
    ]);
    assert!(matches!(
        merge_decision(&input),
        MergeDecision::WaitForReview { .. }
    ));
}

#[test]
fn token_and_credential_changes_remain_outside_the_auto_merge_gate() {
    let policy = RepositoryPolicy {
        auto_merge_enabled: true,
        required_checks: vec!["CI".to_owned()],
        auto_merge_paths: vec!["docs/".to_owned()],
        max_auto_merge_changed_lines: 40,
    };
    let diff = "diff --git a/docs/example.md b/docs/example.md\n@@ -1 +1 @@\n-old\n+access_token: sample\n";

    assert_eq!(classify_risk(diff, &policy).level, RiskLevel::HumanReview);
}

#[test]
fn sensitive_paths_require_human_review_even_when_allowlisted() {
    let cases = [
        ("security", "src/security/policy.rs"),
        ("permissions", "src/permissions.rs"),
        ("migration", "db/migrations/001_add_flag.sql"),
        ("deployment", ".github/workflows/release.yml"),
        ("secret", "config/secrets.toml"),
        ("privacy", "src/privacy.rs"),
        ("public interface", "src/public-api/response.ts"),
    ];

    for (category, path) in cases {
        let policy = RepositoryPolicy {
            auto_merge_enabled: true,
            required_checks: vec!["CI".to_owned()],
            auto_merge_paths: vec![path.to_owned()],
            max_auto_merge_changed_lines: 40,
        };
        let diff = format!("diff --git a/{path} b/{path}\n@@ -1 +1 @@\n-old\n+new\n");

        assert_eq!(
            classify_risk(&diff, &policy).level,
            RiskLevel::HumanReview,
            "{category} changes stay gated even when explicitly allowlisted"
        );
    }
}

#[test]
fn repository_config_can_enable_only_explicit_required_checks_and_paths() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    assert!(!load_repository_policy(root.path())?.auto_merge_enabled);
    std::fs::write(
        root.path().join(".agentic-factory.json"),
        r#"{"github":{"auto_merge_enabled":true,"required_checks":["CI"],"auto_merge_paths":["docs/"],"max_auto_merge_changed_lines":40},"production":{}}"#,
    )?;

    let policy = load_repository_policy(root.path())?;
    assert!(policy.auto_merge_enabled);
    assert_eq!(policy.required_checks, ["CI"]);
    assert_eq!(policy.auto_merge_paths, ["docs/"]);
    assert_eq!(policy.max_auto_merge_changed_lines, 40);
    Ok(())
}

#[test]
fn repository_config_rejects_duplicate_or_unsafe_github_policy() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    std::fs::write(
        root.path().join(".agentic-factory.json"),
        r#"{"github":{"required_checks":["CI","CI"],"auto_merge_paths":["../secrets"]}}"#,
    )?;

    assert!(load_repository_policy(root.path()).is_err());
    Ok(())
}

#[test]
fn an_empty_auto_merge_allowlist_does_not_implicitly_allow_markdown() {
    let policy = RepositoryPolicy {
        auto_merge_enabled: true,
        required_checks: vec!["CI".to_owned()],
        auto_merge_paths: Vec::new(),
        max_auto_merge_changed_lines: 40,
    };
    let diff = "diff --git a/README.md b/README.md\n@@ -1 +1 @@\n-before\n+after\n";

    assert_eq!(classify_risk(diff, &policy).level, RiskLevel::HumanReview);
}

#[test]
fn pull_request_body_uses_durable_verification_review_decision_and_commit_evidence() {
    let body = render_pull_request_body(&PullRequestEvidence {
        change_summary: "Fix search filtering".to_owned(),
        verification: vec!["cargo test: passed".to_owned()],
        independent_review: vec!["reviewer approved head-1".to_owned()],
        decisions: vec!["Keep the API response backward compatible".to_owned()],
        limitations: vec!["No production observation yet".to_owned()],
        worktree_commits: vec!["agent source a1b2c3d -> integration d4e5f6a".to_owned()],
    });

    for evidence in [
        "Fix search filtering",
        "cargo test: passed",
        "reviewer approved head-1",
        "Keep the API response backward compatible",
        "No production observation yet",
        "agent source a1b2c3d -> integration d4e5f6a",
    ] {
        assert!(
            body.contains(evidence),
            "missing PR body evidence: {evidence}"
        );
    }
}
