use crate::{
    policy::{
        classify_risk, merge_decision, render_pull_request_body, CheckEvidence, CheckState,
        GateInput, MergeDecision, PullRequestEvidence, RepositoryPolicy, ReviewEvidence,
        ReviewState, RiskDecision,
    },
    Ledger, RepoId, Repository, RunId, RunRecord,
};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestStatus {
    Open,
    Closed,
    Merged,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PullRequestState {
    pub number: u64,
    pub url: String,
    pub title: String,
    pub status: PullRequestStatus,
    pub is_draft: bool,
    #[serde(default)]
    pub head_branch: String,
    #[serde(default)]
    pub base_branch: String,
    pub head_sha: String,
    pub base_sha: String,
    pub author_login: String,
    pub reviews: Vec<ReviewEvidence>,
    pub checks: Vec<CheckEvidence>,
    pub merged_sha: Option<String>,
    pub observed_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExpectedPullRequestHead {
    pub head_branch: String,
    pub base_branch: String,
    pub head_sha: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PullRequestRecord {
    pub run_id: RunId,
    pub repo_id: RepoId,
    #[serde(flatten)]
    pub state: PullRequestState,
    pub gate: Option<MergeDecision>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GitHubActionRecord {
    pub idempotency_key: String,
    pub run_id: RunId,
    pub action_kind: String,
    pub completed: bool,
    pub result_json: Option<String>,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MergeAttempt {
    pub decision: MergeDecision,
    pub pull_request: PullRequestRecord,
    pub merged: bool,
}

#[derive(Clone, Debug)]
pub struct GitHubCli {
    executable: PathBuf,
}

impl Default for GitHubCli {
    fn default() -> Self {
        Self::new()
    }
}

impl GitHubCli {
    pub fn new() -> Self {
        Self {
            executable: PathBuf::from("gh"),
        }
    }

    pub fn with_executable(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
        }
    }

    pub fn observe_pr(&self, repo_slug: &str, number: u64) -> Result<PullRequestState> {
        validate_repo_slug(repo_slug)?;
        let initial_view = self.pull_request_view(repo_slug, number)?;
        let checks = self.run_check_list(&[
            "-R".to_owned(),
            repo_slug.to_owned(),
            "pr".to_owned(),
            "checks".to_owned(),
            number.to_string(),
            "--json".to_owned(),
            "name,state,bucket".to_owned(),
        ])?;
        // The CLI exposes checks for the current PR head, but does not include that SHA in
        // each row. Confirm the head after the check query so the evidence is bound to a stable
        // commit. A moving PR is a safe, retryable observation failure.
        let confirmed_view = self.pull_request_view(repo_slug, number)?;
        let initial = parse_pull_request(&initial_view, "[]")?;
        let confirmed = parse_pull_request(&confirmed_view, &checks)?;
        anyhow::ensure!(
            initial.number == confirmed.number && initial.head_sha == confirmed.head_sha,
            "pull request head changed while required checks were being read"
        );
        Ok(confirmed)
    }

    fn pull_request_view(&self, repo_slug: &str, number: u64) -> Result<String> {
        let number = number.to_string();
        self.run(&[
            "-R".to_owned(),
            repo_slug.to_owned(),
            "pr".to_owned(),
            "view".to_owned(),
            number,
            "--json".to_owned(),
            "number,url,title,state,isDraft,headRefName,baseRefName,headRefOid,baseRefOid,author,latestReviews,mergeCommit".to_owned(),
        ])
    }

    pub fn pull_request_diff(&self, repo_slug: &str, number: u64) -> Result<String> {
        validate_repo_slug(repo_slug)?;
        self.run(&[
            "-R".to_owned(),
            repo_slug.to_owned(),
            "pr".to_owned(),
            "diff".to_owned(),
            number.to_string(),
        ])
    }

    pub fn create_pr(
        &self,
        repo_slug: &str,
        head_branch: &str,
        base_branch: &str,
        title: &str,
        body: &str,
    ) -> Result<String> {
        validate_repo_slug(repo_slug)?;
        validate_branch(head_branch)?;
        validate_branch(base_branch)?;
        anyhow::ensure!(!title.trim().is_empty(), "pull request title is empty");
        let output = self.run(&[
            "-R".to_owned(),
            repo_slug.to_owned(),
            "pr".to_owned(),
            "create".to_owned(),
            "--head".to_owned(),
            head_branch.to_owned(),
            "--base".to_owned(),
            base_branch.to_owned(),
            "--title".to_owned(),
            title.trim().to_owned(),
            "--body".to_owned(),
            crate::RedactedOutput::new(body).as_str().to_owned(),
        ])?;
        let url = output.trim().to_owned();
        anyhow::ensure!(
            url.starts_with("https://github.com/"),
            "GitHub CLI returned an invalid pull request URL"
        );
        Ok(url)
    }

    fn merge_pr(&self, repo_slug: &str, number: u64, head_sha: &str) -> Result<()> {
        validate_repo_slug(repo_slug)?;
        anyhow::ensure!(
            is_sha(head_sha),
            "refusing to merge without a full current head SHA"
        );
        self.run(&[
            "-R".to_owned(),
            repo_slug.to_owned(),
            "pr".to_owned(),
            "merge".to_owned(),
            number.to_string(),
            "--match-head-commit".to_owned(),
            head_sha.to_owned(),
            "--squash".to_owned(),
        ])?;
        Ok(())
    }

    fn run(&self, args: &[String]) -> Result<String> {
        let output = Command::new(&self.executable)
            .args(args)
            .output()
            .with_context(|| format!("starting GitHub CLI at {}", self.executable.display()))?;
        if !output.status.success() {
            let stderr = crate::RedactedOutput::new(String::from_utf8_lossy(&output.stderr));
            bail!("GitHub CLI command failed: {}", stderr.as_str());
        }
        String::from_utf8(output.stdout).context("GitHub CLI returned non-UTF-8 output")
    }

    fn run_check_list(&self, args: &[String]) -> Result<String> {
        let output = Command::new(&self.executable)
            .args(args)
            .output()
            .with_context(|| format!("starting GitHub CLI at {}", self.executable.display()))?;
        let exit_code = output.status.code();
        anyhow::ensure!(
            output.status.success() || matches!(exit_code, Some(1 | 8)),
            "GitHub CLI command failed: {}",
            crate::RedactedOutput::new(String::from_utf8_lossy(&output.stderr)).as_str()
        );
        let checks =
            String::from_utf8(output.stdout).context("GitHub CLI returned non-UTF-8 check data")?;
        let parsed: Value = serde_json::from_str(&checks)
            .context("decoding GitHub CLI check data despite its status exit code")?;
        anyhow::ensure!(
            parsed.is_array(),
            "GitHub CLI returned an invalid check list"
        );
        Ok(checks)
    }
}

pub fn observe_pr(
    client: &GitHubCli,
    ledger: &Ledger,
    run_id: RunId,
    repository: &Repository,
    number: u64,
    expected_head: &ExpectedPullRequestHead,
) -> Result<PullRequestRecord> {
    ensure_registered_repository(ledger, run_id, repository)?;
    let repo_slug = repository_slug(repository)?;
    let state = client.observe_pr(&repo_slug, number)?;
    validate_expected_pull_request_head(&state, expected_head)?;
    let record = PullRequestRecord {
        run_id,
        repo_id: repository.id,
        state,
        gate: None,
    };
    ledger.store_pull_request(&record)?;
    Ok(record)
}

pub fn evaluate_pull_request_gate(
    client: &GitHubCli,
    ledger: &Ledger,
    run: &RunRecord,
    repository: &Repository,
    policy: &RepositoryPolicy,
    expected_head: &ExpectedPullRequestHead,
    number: u64,
) -> Result<PullRequestRecord> {
    ensure_registered_run(ledger, run)?;
    ensure_run_repository(run, repository)?;
    validate_expected_head(expected_head)?;
    let repo_slug = repository_slug(repository)?;
    let state = client.observe_pr(&repo_slug, number)?;
    if let Some(reason) = pull_request_head_mismatch(&state, expected_head) {
        let already_tracked = ledger.latest_pull_request(run.id)?.is_some_and(|tracked| {
            tracked.state.number == state.number && tracked.state.url == state.url
        });
        if !already_tracked {
            bail!(reason);
        }
        let record = record_with_gate(run, repository, state, MergeDecision::Block { reason });
        ledger.store_pull_request(&record)?;
        return Ok(record);
    }

    let diff = client.pull_request_diff(&repo_slug, number)?;
    let risk = classify_risk(&diff, policy);
    let gate = ensure_production_watch(
        repository,
        evaluate(&state, policy, risk, &expected_head.head_sha),
    );
    let record = record_with_gate(run, repository, state, gate);
    ledger.store_pull_request(&record)?;
    Ok(record)
}

pub fn create_pull_request(
    client: &GitHubCli,
    ledger: &Ledger,
    run: &RunRecord,
    repository: &Repository,
    expected_head: &ExpectedPullRequestHead,
    idempotency_key: &str,
) -> Result<PullRequestRecord> {
    ensure_registered_run(ledger, run)?;
    ensure_run_repository(run, repository)?;
    if let Some(action) = ledger.github_action(idempotency_key)? {
        if action.run_id != run.id || action.action_kind != "create_pull_request" {
            bail!("GitHub idempotency key belongs to a different action");
        }
        if action.completed {
            return serde_json::from_str(&action.result_json.unwrap_or_default())
                .context("decoding the recorded pull request creation result");
        }
        bail!("a previous pull request creation attempt is unresolved; observe it before retrying");
    }

    validate_expected_head(expected_head)?;
    let repo_slug = repository_slug(repository)?;
    let evidence = ledger
        .pull_request_evidence(run.id)?
        .unwrap_or_else(|| PullRequestEvidence {
            change_summary: run.title.clone(),
            ..PullRequestEvidence::default()
        });
    let body = render_pull_request_body(&evidence);
    let inserted = ledger.begin_github_action(idempotency_key, run.id, "create_pull_request")?;
    if !inserted {
        bail!("a previous pull request creation attempt is unresolved; observe it before retrying");
    }
    let url = client.create_pr(
        &repo_slug,
        &expected_head.head_branch,
        &expected_head.base_branch,
        &run.title,
        &body,
    )?;
    let number = pull_request_number(&url)?;
    let state = client.observe_pr(&repo_slug, number)?;
    validate_expected_pull_request_head(&state, expected_head)?;
    let record = PullRequestRecord {
        run_id: run.id,
        repo_id: repository.id,
        state,
        gate: None,
    };
    ledger.store_pull_request(&record)?;
    ledger.complete_github_action(idempotency_key, &serde_json::to_string(&record)?)?;
    Ok(record)
}

pub fn try_merge_pull_request(
    client: &GitHubCli,
    ledger: &Ledger,
    run: &RunRecord,
    repository: &Repository,
    number: u64,
    expected_head: &ExpectedPullRequestHead,
    policy: &RepositoryPolicy,
    idempotency_key: &str,
) -> Result<MergeAttempt> {
    ensure_registered_run(ledger, run)?;
    ensure_run_repository(run, repository)?;
    if let Some(action) = ledger.github_action(idempotency_key)? {
        if action.run_id != run.id || action.action_kind != "merge_pull_request" {
            bail!("GitHub idempotency key belongs to a different action");
        }
        if action.completed {
            return serde_json::from_str(&action.result_json.unwrap_or_default())
                .context("decoding the recorded pull request merge result");
        }
        bail!("a previous merge attempt is unresolved; refresh the pull request before acting");
    }

    let repo_slug = repository_slug(repository)?;
    let first_record = evaluate_pull_request_gate(
        client,
        ledger,
        run,
        repository,
        policy,
        expected_head,
        number,
    )?;
    let observed = first_record.state.clone();
    let risk = classify_risk(&client.pull_request_diff(&repo_slug, number)?, policy);
    let first_decision = first_record
        .gate
        .clone()
        .unwrap_or_else(|| MergeDecision::Block {
            reason: "merge gate did not produce a decision".to_owned(),
        });
    if first_decision != MergeDecision::AutoMerge {
        let record = record_with_gate(run, repository, observed, first_decision.clone());
        ledger.store_pull_request(&record)?;
        return Ok(MergeAttempt {
            decision: first_decision,
            pull_request: record,
            merged: false,
        });
    }

    // Re-read both the PR head and the required checks directly before requesting the merge.
    let fresh = client.observe_pr(&repo_slug, number)?;
    let decision = if let Some(reason) = pull_request_head_mismatch(&fresh, expected_head) {
        MergeDecision::Block { reason }
    } else {
        ensure_production_watch(
            repository,
            evaluate(&fresh, policy, risk, &expected_head.head_sha),
        )
    };
    let mut record = record_with_gate(run, repository, fresh, decision.clone());
    ledger.store_pull_request(&record)?;
    if decision != MergeDecision::AutoMerge {
        return Ok(MergeAttempt {
            decision,
            pull_request: record,
            merged: false,
        });
    }

    if !ledger.begin_github_action(idempotency_key, run.id, "merge_pull_request")? {
        bail!("a previous merge attempt is unresolved; refresh the pull request before acting");
    }
    client.merge_pr(&repo_slug, number, &record.state.head_sha)?;
    let after_merge = client.observe_pr(&repo_slug, number)?;
    record = record_with_gate(run, repository, after_merge, MergeDecision::AutoMerge);
    ledger.store_pull_request(&record)?;
    let result = MergeAttempt {
        decision: MergeDecision::AutoMerge,
        merged: record.state.status == PullRequestStatus::Merged,
        pull_request: record,
    };
    ledger.complete_github_action(idempotency_key, &serde_json::to_string(&result)?)?;
    Ok(result)
}

fn evaluate(
    pull_request: &PullRequestState,
    policy: &RepositoryPolicy,
    risk: RiskDecision,
    expected_head_sha: &str,
) -> MergeDecision {
    merge_decision(&GateInput {
        pr_open: pull_request.status == PullRequestStatus::Open,
        pr_draft: pull_request.is_draft,
        expected_head_sha: expected_head_sha.to_owned(),
        observed_head_sha: pull_request.head_sha.clone(),
        author_login: pull_request.author_login.clone(),
        reviews: pull_request.reviews.clone(),
        checks: pull_request.checks.clone(),
        required_checks: policy.required_checks.clone(),
        risk,
        auto_merge_enabled: policy.auto_merge_enabled,
    })
}

fn record_with_gate(
    run: &RunRecord,
    repository: &Repository,
    state: PullRequestState,
    gate: MergeDecision,
) -> PullRequestRecord {
    PullRequestRecord {
        run_id: run.id,
        repo_id: repository.id,
        state,
        gate: Some(gate),
    }
}

fn ensure_run_repository(run: &RunRecord, repository: &Repository) -> Result<()> {
    anyhow::ensure!(
        run.repo_id == repository.id,
        "pull request repository does not match its run"
    );
    Ok(())
}

fn ensure_registered_run(ledger: &Ledger, run: &RunRecord) -> Result<()> {
    let registered = ledger
        .list_runs()?
        .into_iter()
        .find(|registered| registered.id == run.id)
        .ok_or_else(|| anyhow!("pull request run is not registered"))?;
    anyhow::ensure!(
        registered == *run,
        "pull request run details do not match the ledger"
    );
    Ok(())
}

fn ensure_registered_repository(
    ledger: &Ledger,
    run_id: RunId,
    repository: &Repository,
) -> Result<()> {
    let registered = ledger
        .list_runs()?
        .into_iter()
        .find(|run| run.id == run_id)
        .ok_or_else(|| anyhow!("pull request run is not registered"))?;
    anyhow::ensure!(
        registered.repo_id == repository.id,
        "pull request repository does not match its run"
    );
    Ok(())
}

fn ensure_production_watch(repository: &Repository, decision: MergeDecision) -> MergeDecision {
    if decision != MergeDecision::AutoMerge {
        return decision;
    }
    match crate::load_production_policy(&repository.canonical_root) {
        Ok(policy) if policy.identity_url.is_some() && policy.smoke_url.is_some() => decision,
        Ok(_) => MergeDecision::WaitForReview {
            reason: "valid production watch configuration requires both identity_url and smoke_url before auto-merge".to_owned(),
        },
        Err(error) => MergeDecision::WaitForReview {
            reason: format!(
                "valid production watch configuration is required before auto-merge: {}",
                crate::RedactedOutput::new(error.to_string()).as_str()
            ),
        },
    }
}

fn validate_expected_head(expected: &ExpectedPullRequestHead) -> Result<()> {
    validate_branch(&expected.head_branch)?;
    validate_branch(&expected.base_branch)?;
    anyhow::ensure!(
        is_sha(&expected.head_sha),
        "expected pull request head must be a full commit SHA"
    );
    Ok(())
}

fn validate_expected_pull_request_head(
    pull_request: &PullRequestState,
    expected: &ExpectedPullRequestHead,
) -> Result<()> {
    if let Some(reason) = pull_request_head_mismatch(pull_request, expected) {
        bail!("{reason}");
    }
    Ok(())
}

fn pull_request_head_mismatch(
    pull_request: &PullRequestState,
    expected: &ExpectedPullRequestHead,
) -> Option<String> {
    if pull_request.head_branch != expected.head_branch {
        Some("pull request head branch does not match this run's integration branch".to_owned())
    } else if pull_request.base_branch != expected.base_branch {
        Some("pull request base branch does not match the repository default branch".to_owned())
    } else if pull_request.head_sha != expected.head_sha {
        Some("pull request head commit does not match this run's integration commit".to_owned())
    } else {
        None
    }
}

fn parse_pull_request(view: &str, checks: &str) -> Result<PullRequestState> {
    let view: Value = serde_json::from_str(view).context("decoding GitHub pull request data")?;
    let number = view
        .get("number")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("GitHub response has no pull request number"))?;
    let url = json_string(&view, "url")?;
    let title = crate::RedactedOutput::new(json_string(&view, "title")?)
        .as_str()
        .to_owned();
    let status = match json_string(&view, "state")?.to_ascii_uppercase().as_str() {
        "OPEN" => PullRequestStatus::Open,
        "MERGED" => PullRequestStatus::Merged,
        "CLOSED" => PullRequestStatus::Closed,
        _ => bail!("GitHub response has an unknown pull request state"),
    };
    let is_draft = view
        .get("isDraft")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let head_sha = optional_json_string(&view, "headRefOid");
    let base_sha = optional_json_string(&view, "baseRefOid");
    let head_branch = optional_json_string(&view, "headRefName");
    let base_branch = optional_json_string(&view, "baseRefName");
    let author_login = view
        .get("author")
        .and_then(|author| author.get("login"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let reviews = view
        .get("latestReviews")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, review)| {
            let author_login = review
                .get("author")
                .and_then(|author| author.get("login"))
                .and_then(Value::as_str)?;
            let state = parse_review_state(review.get("state")?.as_str()?)?;
            Some(ReviewEvidence {
                author_login: author_login.to_owned(),
                state,
                head_sha: review
                    .get("commit")
                    .and_then(|commit| commit.get("oid"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                // `latestReviews` contains at most one decision per reviewer. Its order is
                // therefore sufficient for deterministic tie-breaking within this response.
                submitted_at_ms: index as i64,
            })
        })
        .collect();
    let checks_json: Value = serde_json::from_str(checks).context("decoding GitHub check data")?;
    let checks = checks_json
        .as_array()
        .ok_or_else(|| anyhow!("GitHub CLI returned an invalid check list"))?
        .iter()
        .filter_map(|check| {
            Some(CheckEvidence {
                name: check.get("name")?.as_str()?.to_owned(),
                state: parse_check_state(
                    check.get("bucket").and_then(Value::as_str),
                    check.get("state").and_then(Value::as_str),
                ),
                head_sha: head_sha.clone(),
            })
        })
        .collect();
    let merged_sha = view
        .get("mergeCommit")
        .and_then(|commit| commit.get("oid"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(PullRequestState {
        number,
        url,
        title,
        status,
        is_draft,
        head_branch,
        base_branch,
        head_sha,
        base_sha,
        author_login,
        reviews,
        checks,
        merged_sha,
        observed_at_ms: now_ms(),
    })
}

fn parse_review_state(value: &str) -> Option<ReviewState> {
    match value.to_ascii_uppercase().as_str() {
        "APPROVED" => Some(ReviewState::Approved),
        "CHANGES_REQUESTED" => Some(ReviewState::ChangesRequested),
        "COMMENTED" => Some(ReviewState::Commented),
        "DISMISSED" => Some(ReviewState::Dismissed),
        _ => None,
    }
}

fn parse_check_state(bucket: Option<&str>, state: Option<&str>) -> CheckState {
    match bucket.unwrap_or_default().to_ascii_lowercase().as_str() {
        "pass" => CheckState::Passed,
        "fail" => CheckState::Failed,
        "pending" => CheckState::Pending,
        "skipping" => CheckState::Skipped,
        "cancel" => CheckState::Cancelled,
        _ => match state.unwrap_or_default().to_ascii_uppercase().as_str() {
            "SUCCESS" => CheckState::Passed,
            "FAILURE" | "ERROR" => CheckState::Failed,
            "PENDING" | "QUEUED" | "IN_PROGRESS" => CheckState::Pending,
            "SKIPPED" => CheckState::Skipped,
            "CANCELLED" | "CANCELED" => CheckState::Cancelled,
            _ => CheckState::Pending,
        },
    }
}

fn json_string(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("GitHub response has no {key}"))
}

fn optional_json_string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn validate_repo_slug(value: &str) -> Result<()> {
    let parts = value.split('/').collect::<Vec<_>>();
    anyhow::ensure!(
        parts.len() == 2 || parts.len() == 3,
        "GitHub repository must be a host/owner/name or owner/name slug"
    );
    anyhow::ensure!(
        parts.len() != 3 || parts[0].eq_ignore_ascii_case("github.com"),
        "GitHub repository slug cannot override the GitHub host"
    );
    anyhow::ensure!(
        parts.iter().all(|part| {
            !part.is_empty()
                && part.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
                })
        }),
        "GitHub repository slug contains an invalid character"
    );
    Ok(())
}

fn validate_branch(value: &str) -> Result<()> {
    anyhow::ensure!(
        !value.is_empty()
            && value.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '/' | '.' | '_' | '-')
            }),
        "Git branch contains an invalid character"
    );
    Ok(())
}

fn repository_slug(repository: &Repository) -> Result<String> {
    let remote = repository
        .remote_url
        .as_deref()
        .ok_or_else(|| anyhow!("repository has no GitHub remote"))?;
    github_repo_slug(remote)
}

pub fn github_repo_slug(remote: &str) -> Result<String> {
    let remote = remote.split(['?', '#']).next().unwrap_or_default();
    let path = if let Some((scheme, rest)) = remote.split_once("://") {
        anyhow::ensure!(
            matches!(scheme, "https" | "ssh" | "git"),
            "repository remote must use HTTPS, SSH, or Git"
        );
        let rest = rest.strip_prefix("git@").unwrap_or(rest);
        let (host, path) = rest
            .split_once('/')
            .ok_or_else(|| anyhow!("GitHub remote has no repository path"))?;
        anyhow::ensure!(
            host.eq_ignore_ascii_case("github.com"),
            "repository remote is not hosted on github.com"
        );
        path
    } else if let Some((authority, path)) = remote.split_once(':') {
        let host = authority.rsplit('@').next().unwrap_or(authority);
        anyhow::ensure!(
            host.eq_ignore_ascii_case("github.com"),
            "repository remote is not hosted on github.com"
        );
        path
    } else {
        bail!("repository remote is not a recognized GitHub URL");
    };
    let path = path.trim_end_matches(".git").trim_matches('/');
    let parts = path.split('/').collect::<Vec<_>>();
    anyhow::ensure!(
        parts.len() == 2,
        "GitHub remote must identify an owner and repository"
    );
    let slug = format!("{}/{}", parts[0], parts[1]);
    validate_repo_slug(&slug)?;
    Ok(slug)
}

fn pull_request_number(url: &str) -> Result<u64> {
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .ok_or_else(|| anyhow!("pull request URL has no number"))?
        .parse()
        .context("parsing pull request number from its URL")
}

fn is_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::PermissionsExt, path::Path};

    fn write_fake_gh(path: &Path, body: &str) -> anyhow::Result<()> {
        std::fs::write(path, format!("#!/bin/sh\nset -eu\n{body}"))?;
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(path, permissions)?;
        Ok(())
    }

    #[test]
    fn observation_rejects_a_head_change_while_required_checks_are_read() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let executable = temp.path().join("gh");
        let counter = temp.path().join("calls");
        let script = r#"COUNTER='COUNTER_PATH'
count=0
if [ -f "$COUNTER" ]; then count=$(cat "$COUNTER"); fi
count=$((count + 1))
printf '%s' "$count" > "$COUNTER"
if [ "$4" = "checks" ]; then
  printf '%s' '[]'
elif [ "$count" = "1" ]; then
  printf '%s' '{"number":7,"url":"https://github.com/example/repo/pull/7","title":"Fixture","state":"OPEN","isDraft":false,"headRefOid":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","baseRefOid":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","author":{"login":"author"},"latestReviews":[],"mergeCommit":null}'
else
  printf '%s' '{"number":7,"url":"https://github.com/example/repo/pull/7","title":"Fixture","state":"OPEN","isDraft":false,"headRefOid":"cccccccccccccccccccccccccccccccccccccccc","baseRefOid":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","author":{"login":"author"},"latestReviews":[],"mergeCommit":null}'
fi
"#
        .replace("COUNTER_PATH", &counter.display().to_string());
        write_fake_gh(&executable, &script)?;

        let result = GitHubCli::with_executable(executable).observe_pr("example/repo", 7);
        assert!(
            result.is_err(),
            "a check list cannot be attached to a different head"
        );
        assert_eq!(std::fs::read_to_string(counter)?, "3");
        Ok(())
    }

    #[test]
    fn pending_checks_are_parsed_even_when_gh_returns_its_pending_exit_code() -> anyhow::Result<()>
    {
        let temp = tempfile::tempdir()?;
        let executable = temp.path().join("gh");
        let head = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let script = format!(
            r#"if [ "$4" = "checks" ]; then
  printf '%s' '[{{"name":"verify","state":"IN_PROGRESS","bucket":"pending"}}]'
  exit 8
fi
printf '%s' '{{"number":7,"url":"https://github.com/example/repo/pull/7","title":"Fixture","state":"OPEN","isDraft":false,"headRefOid":"{head}","baseRefOid":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","author":{{"login":"author"}},"latestReviews":[],"mergeCommit":null}}'
"#
        );
        write_fake_gh(&executable, &script)?;

        let pull_request = GitHubCli::with_executable(executable).observe_pr("example/repo", 7)?;
        assert_eq!(pull_request.checks.len(), 1);
        assert_eq!(pull_request.checks[0].state, CheckState::Pending);
        assert_eq!(pull_request.checks[0].head_sha, head);
        Ok(())
    }

    #[test]
    fn failed_checks_are_parsed_even_when_gh_returns_exit_code_one() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let executable = temp.path().join("gh");
        let head = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let script = format!(
            r#"if [ "$4" = "checks" ]; then
  printf '%s' '[{{"name":"verify","state":"FAILURE","bucket":"fail"}}]'
  exit 1
fi
printf '%s' '{{"number":7,"url":"https://github.com/example/repo/pull/7","title":"Fixture","state":"OPEN","isDraft":false,"headRefOid":"{head}","baseRefOid":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","author":{{"login":"author"}},"latestReviews":[],"mergeCommit":null}}'
"#
        );
        write_fake_gh(&executable, &script)?;

        let pull_request = GitHubCli::with_executable(executable).observe_pr("example/repo", 7)?;
        assert_eq!(pull_request.checks.len(), 1);
        assert_eq!(pull_request.checks[0].state, CheckState::Failed);
        assert_eq!(pull_request.checks[0].head_sha, head);
        Ok(())
    }

    #[test]
    fn observation_includes_checks_that_are_required_only_by_app_policy() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let executable = temp.path().join("gh");
        let head = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let script = format!(
            r#"if [ "$4" = "checks" ]; then
  case " $* " in
    *" --required "*) printf '%s' '[{{"name":"github-required","state":"SUCCESS","bucket":"pass"}}]' ;;
    *) printf '%s' '[{{"name":"github-required","state":"SUCCESS","bucket":"pass"}},{{"name":"app-only-required","state":"SUCCESS","bucket":"pass"}}]' ;;
  esac
  exit 0
fi
printf '%s' '{{"number":7,"url":"https://github.com/example/repo/pull/7","title":"Fixture","state":"OPEN","isDraft":false,"headRefOid":"{head}","baseRefOid":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","author":{{"login":"author"}},"latestReviews":[],"mergeCommit":null}}'
"#
        );
        write_fake_gh(&executable, &script)?;

        let pull_request = GitHubCli::with_executable(executable).observe_pr("example/repo", 7)?;
        assert_eq!(pull_request.checks.len(), 2);
        assert!(pull_request
            .checks
            .iter()
            .any(|check| check.name == "app-only-required"));
        Ok(())
    }

    #[test]
    fn auto_merge_requires_both_production_observation_urls() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let repository = Repository {
            id: RepoId::new(),
            canonical_root: temp.path().to_path_buf(),
            remote_url: Some("https://github.com/example/repo.git".to_owned()),
            default_branch: "main".to_owned(),
            registered_at_ms: 1,
        };
        let config_path = temp.path().join(".agentic-factory.json");
        std::fs::write(
            &config_path,
            r#"{"production":{"environment_id":"staging"}}"#,
        )?;
        let incomplete = ensure_production_watch(&repository, MergeDecision::AutoMerge);
        assert!(matches!(incomplete, MergeDecision::WaitForReview { .. }));

        std::fs::write(
            &config_path,
            r#"{"production":{"environment_id":"staging","identity_url":"https://status.example.com/build.json","smoke_url":"https://status.example.com/health"}}"#,
        )?;
        assert_eq!(
            ensure_production_watch(&repository, MergeDecision::AutoMerge),
            MergeDecision::AutoMerge
        );
        Ok(())
    }

    #[test]
    fn explicit_host_slugs_cannot_redirect_gh_to_an_untrusted_host() {
        assert!(validate_repo_slug("evil.example/owner/repo").is_err());
        assert!(validate_repo_slug("github.com/owner/repo").is_ok());
    }

    #[test]
    fn pull_request_observation_binds_reviews_and_checks_to_the_observed_head() {
        let head = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let view = format!(
            r#"{{"number":7,"url":"https://github.com/example/repo/pull/7","title":"Fixture","state":"OPEN","isDraft":false,"headRefOid":"{head}","baseRefOid":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","author":{{"login":"author"}},"latestReviews":[{{"author":{{"login":"reviewer"}},"state":"APPROVED","commit":{{"oid":"{head}"}}}}],"mergeCommit":null}}"#
        );
        let checks = r#"[{"name":"verify","state":"SUCCESS","bucket":"pass"}]"#;

        let pull_request = parse_pull_request(&view, checks).expect("fixture data is valid");
        assert_eq!(pull_request.reviews[0].head_sha, head);
        assert_eq!(pull_request.checks[0].head_sha, head);
        assert_eq!(pull_request.checks[0].state, CheckState::Passed);
    }

    #[test]
    fn older_pull_request_records_without_branch_names_still_deserialize() {
        let old_record = r#"{"number":7,"url":"https://github.com/example/repo/pull/7","title":"Fixture","status":"open","is_draft":false,"head_sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","base_sha":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","author_login":"author","reviews":[],"checks":[],"merged_sha":null,"observed_at_ms":1}"#;

        let state: PullRequestState =
            serde_json::from_str(old_record).expect("older records remain readable");
        assert_eq!(state.head_branch, "");
        assert_eq!(state.base_branch, "");
    }

    #[test]
    fn github_remotes_are_normalized_and_other_hosts_are_rejected() {
        assert_eq!(
            github_repo_slug("https://github.com/owner/repo.git").unwrap(),
            "owner/repo"
        );
        assert_eq!(
            github_repo_slug("git@github.com:owner/repo.git").unwrap(),
            "owner/repo"
        );
        assert!(github_repo_slug("https://user:token@github.com/owner/repo.git").is_err());
        assert!(github_repo_slug("https://github.example/owner/repo.git").is_err());
    }
}
