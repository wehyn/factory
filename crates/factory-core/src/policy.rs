use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::Path, sync::OnceLock};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct RepositoryPolicy {
    pub auto_merge_enabled: bool,
    pub required_checks: Vec<String>,
    /// Explicit allowlist. Entries support exact paths, directory prefixes ending in `/`,
    /// and filename suffixes such as `*.md`.
    pub auto_merge_paths: Vec<String>,
    pub max_auto_merge_changed_lines: usize,
}

impl Default for RepositoryPolicy {
    fn default() -> Self {
        Self {
            auto_merge_enabled: false,
            required_checks: Vec::new(),
            auto_merge_paths: vec!["docs/".to_owned(), "*.md".to_owned(), "*.mdx".to_owned()],
            max_auto_merge_changed_lines: 100,
        }
    }
}

/// Reads the optional repository policy from `.agentic-factory.json`. An absent file keeps
/// auto-merge disabled; malformed or unsafe policy is an error and must not be ignored.
pub fn load_repository_policy(repository_root: &Path) -> Result<RepositoryPolicy> {
    let path = repository_root.join(".agentic-factory.json");
    if !path.exists() {
        return Ok(RepositoryPolicy::default());
    }
    let bytes = std::fs::read(&path)
        .with_context(|| format!("reading repository policy at {}", path.display()))?;
    let config: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("decoding repository config at {}", path.display()))?;
    anyhow::ensure!(
        config.is_object(),
        "repository config must be a JSON object"
    );
    let github = config
        .get("github")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let policy = if github.is_null() {
        RepositoryPolicy::default()
    } else {
        serde_json::from_value::<RepositoryPolicy>(github)
            .with_context(|| format!("decoding GitHub policy at {}", path.display()))?
    };
    validate_repository_policy(&policy)?;
    Ok(policy)
}

fn validate_repository_policy(policy: &RepositoryPolicy) -> Result<()> {
    anyhow::ensure!(
        policy.max_auto_merge_changed_lines > 0,
        "GitHub policy max_auto_merge_changed_lines must be greater than zero"
    );
    anyhow::ensure!(
        policy
            .required_checks
            .iter()
            .all(|check| !check.trim().is_empty()),
        "GitHub policy required_checks cannot contain empty names"
    );
    let mut names = policy.required_checks.clone();
    names.sort();
    names.dedup();
    anyhow::ensure!(
        names.len() == policy.required_checks.len(),
        "GitHub policy required_checks cannot contain duplicates"
    );
    anyhow::ensure!(
        policy.auto_merge_paths.iter().all(|path| {
            !path.trim().is_empty()
                && !path.starts_with('/')
                && !path.split('/').any(|segment| segment == "..")
        }),
        "GitHub policy auto_merge_paths must be non-empty repository-relative paths"
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    HumanReview,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RiskDecision {
    pub level: RiskLevel,
    pub reasons: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReviewEvidence {
    pub author_login: String,
    pub state: ReviewState,
    /// Commit to which the reviewer submitted this decision. An empty value is stale evidence.
    pub head_sha: String,
    pub submitted_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    Passed,
    Failed,
    Pending,
    Skipped,
    Cancelled,
    Missing,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CheckEvidence {
    pub name: String,
    pub state: CheckState,
    /// The PR head captured immediately before this check list was read.
    pub head_sha: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GateInput {
    pub pr_open: bool,
    pub pr_draft: bool,
    pub expected_head_sha: String,
    pub observed_head_sha: String,
    pub author_login: String,
    pub reviews: Vec<ReviewEvidence>,
    pub checks: Vec<CheckEvidence>,
    pub required_checks: Vec<String>,
    pub risk: RiskDecision,
    pub auto_merge_enabled: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "reason", rename_all = "snake_case")]
pub enum MergeDecision {
    AutoMerge,
    WaitForReview { reason: String },
    Block { reason: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct PullRequestEvidenceInput {
    pub change_summary: String,
    pub verification: Vec<String>,
    pub independent_review: Vec<String>,
    pub decisions: Vec<String>,
    pub limitations: Vec<String>,
}

impl From<PullRequestEvidenceInput> for PullRequestEvidence {
    fn from(input: PullRequestEvidenceInput) -> Self {
        Self {
            change_summary: input.change_summary,
            verification: input.verification,
            independent_review: input.independent_review,
            decisions: input.decisions,
            limitations: input.limitations,
            worktree_commits: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PullRequestEvidence {
    pub change_summary: String,
    pub verification: Vec<String>,
    pub independent_review: Vec<String>,
    pub decisions: Vec<String>,
    pub limitations: Vec<String>,
    #[serde(default)]
    pub worktree_commits: Vec<String>,
}

pub fn classify_risk(diff: &str, policy: &RepositoryPolicy) -> RiskDecision {
    let diff = diff.trim();
    if diff.is_empty() {
        return risk(
            RiskLevel::Unknown,
            "the pull request diff is empty or unavailable",
        );
    }

    let paths = changed_paths(diff);
    if paths.is_empty() {
        return risk(
            RiskLevel::Unknown,
            "changed file paths could not be identified",
        );
    }

    for path in &paths {
        if sensitive_path(path) {
            return risk(
                RiskLevel::HumanReview,
                format!("sensitive area changed: {path}"),
            );
        }
    }
    for line in diff.lines().filter(|line| {
        (line.starts_with('+') && !line.starts_with("+++"))
            || (line.starts_with('-') && !line.starts_with("---"))
    }) {
        if sensitive_change_regex().is_match(&line[1..]) {
            return risk(
                RiskLevel::HumanReview,
                "changed lines touch security, identity, privacy, deployment, secret, migration, or public API behavior",
            );
        }
    }

    let changed_lines = diff
        .lines()
        .filter(|line| {
            (line.starts_with('+') && !line.starts_with("+++"))
                || (line.starts_with('-') && !line.starts_with("---"))
        })
        .count();
    if changed_lines > policy.max_auto_merge_changed_lines {
        return risk(
            RiskLevel::HumanReview,
            format!("change has {changed_lines} added or removed lines"),
        );
    }

    let unapproved = paths
        .iter()
        .find(|path| !is_auto_merge_path(path, &policy.auto_merge_paths));
    if let Some(path) = unapproved {
        return risk(
            RiskLevel::HumanReview,
            format!("path is outside the repository's auto-merge allowlist: {path}"),
        );
    }

    RiskDecision {
        level: RiskLevel::Low,
        reasons: vec![format!(
            "{} changed path(s), {changed_lines} added or removed lines, all within the auto-merge allowlist",
            paths.len()
        )],
    }
}

pub fn merge_decision(input: &GateInput) -> MergeDecision {
    if !input.pr_open || input.pr_draft {
        return MergeDecision::Block {
            reason: "pull request is closed, merged, or still a draft".to_owned(),
        };
    }
    if input.expected_head_sha.is_empty()
        || input.observed_head_sha.is_empty()
        || input.expected_head_sha != input.observed_head_sha
    {
        return MergeDecision::Block {
            reason: "pull request head changed since its evidence was collected".to_owned(),
        };
    }
    if input.required_checks.is_empty() {
        return MergeDecision::Block {
            reason: "repository policy does not declare required checks".to_owned(),
        };
    }
    for required in &input.required_checks {
        let matching = input
            .checks
            .iter()
            .filter(|check| check.name == *required)
            .collect::<Vec<_>>();
        if matching.is_empty() {
            return MergeDecision::Block {
                reason: format!("required check is missing: {required}"),
            };
        }
        if matching
            .iter()
            .any(|check| check.head_sha != input.observed_head_sha)
        {
            return MergeDecision::Block {
                reason: format!("required check is stale for the current head: {required}"),
            };
        }
        if matching
            .iter()
            .any(|check| check.state != CheckState::Passed)
        {
            return MergeDecision::Block {
                reason: format!("required check has not passed: {required}"),
            };
        }
    }

    match input.risk.level {
        RiskLevel::HumanReview => {
            return MergeDecision::WaitForReview {
                reason: input
                    .risk
                    .reasons
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "risk policy requires Wayne's review".to_owned()),
            }
        }
        RiskLevel::Unknown => {
            return MergeDecision::WaitForReview {
                reason: "risk could not be classified conservatively".to_owned(),
            }
        }
        RiskLevel::Low => {}
    }

    let author = input.author_login.to_lowercase();
    let mut latest_reviews = HashMap::<String, &ReviewEvidence>::new();
    for review in &input.reviews {
        let login = review.author_login.to_lowercase();
        if login == author || login.is_empty() || review.head_sha != input.observed_head_sha {
            continue;
        }
        let replace = latest_reviews
            .get(&login)
            .is_none_or(|previous| review.submitted_at_ms >= previous.submitted_at_ms);
        if replace {
            latest_reviews.insert(login, review);
        }
    }
    if latest_reviews
        .values()
        .any(|review| review.state == ReviewState::ChangesRequested)
    {
        return MergeDecision::WaitForReview {
            reason: "an independent reviewer has requested changes".to_owned(),
        };
    }
    if !latest_reviews
        .values()
        .any(|review| review.state == ReviewState::Approved)
    {
        return MergeDecision::WaitForReview {
            reason: "an independent approval for the current pull request is required".to_owned(),
        };
    }
    if !input.auto_merge_enabled {
        return MergeDecision::WaitForReview {
            reason: "automatic merge is disabled by repository policy".to_owned(),
        };
    }
    MergeDecision::AutoMerge
}

pub fn render_pull_request_body(evidence: &PullRequestEvidence) -> String {
    let mut body = String::new();
    body.push_str("## Change summary\n\n");
    body.push_str(&safe_markdown_value(
        &evidence.change_summary,
        "No summary recorded.",
    ));
    append_section(&mut body, "Verification", &evidence.verification);
    append_section(
        &mut body,
        "Independent review",
        &evidence.independent_review,
    );
    append_section(&mut body, "Decisions", &evidence.decisions);
    append_section(&mut body, "Limitations", &evidence.limitations);
    append_section(
        &mut body,
        "Worktree and commit provenance",
        &evidence.worktree_commits,
    );
    body
}

fn append_section(body: &mut String, title: &str, values: &[String]) {
    body.push_str(&format!("\n\n## {title}\n\n"));
    if values.is_empty() {
        body.push_str("Not recorded.");
        return;
    }
    for value in values {
        body.push_str("- ");
        body.push_str(&safe_markdown_value(value, ""));
        body.push('\n');
    }
    body.pop();
}

fn safe_markdown_value(value: &str, fallback: &str) -> String {
    let value = crate::RedactedOutput::new(value).as_str().trim().to_owned();
    if value.is_empty() {
        fallback.to_owned()
    } else {
        value
    }
}

fn risk(level: RiskLevel, reason: impl Into<String>) -> RiskDecision {
    RiskDecision {
        level,
        reasons: vec![reason.into()],
    }
}

fn changed_paths(diff: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in diff.lines() {
        let path = line
            .strip_prefix("+++ b/")
            .or_else(|| line.strip_prefix("--- a/"))
            .or_else(|| line.strip_prefix("rename to "))
            .or_else(|| line.strip_prefix("rename from "));
        if let Some(path) = path {
            if path != "/dev/null" && !paths.iter().any(|seen| seen == path) {
                paths.push(path.to_owned());
            }
        }
        if let Some(rest) = line.strip_prefix("diff --git a/") {
            if let Some((old_path, new_path)) = rest.split_once(" b/") {
                for path in [old_path, new_path] {
                    if !paths.iter().any(|seen| seen == path) {
                        paths.push(path.to_owned());
                    }
                }
            }
        }
    }
    paths
}

fn sensitive_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_lowercase();
    normalized.starts_with(".github/")
        || normalized.contains("/.github/")
        || matches!(
            normalized.rsplit('/').next().unwrap_or_default(),
            "dockerfile" | "vercel.json" | "netlify.toml" | "render.yaml" | "procfile"
        )
        || sensitive_path_regex().is_match(&normalized)
}

fn is_auto_merge_path(path: &str, allowlist: &[String]) -> bool {
    let lower = path.to_lowercase();
    allowlist.iter().any(|pattern| {
        let pattern = pattern.to_lowercase();
        if let Some(prefix) = pattern.strip_suffix("/**") {
            lower.starts_with(&format!("{prefix}/"))
        } else if let Some(prefix) = pattern.strip_suffix('/') {
            lower.starts_with(&format!("{prefix}/"))
        } else if let Some(suffix) = pattern.strip_prefix("*") {
            lower.ends_with(suffix)
        } else {
            lower == pattern
        }
    })
}

fn sensitive_path_regex() -> &'static regex::Regex {
    static REGEX: OnceLock<regex::Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        regex::Regex::new(
            r"(?i)(^|/)(security|auth(?:entication|orization)?|permissions?|privacy|migrations?|deploy(?:ment)?|secrets?|tokens?|credentials?|passwords?|private[-_ ]?keys?|openapi|swagger|public[-_ ]?api|apis?|schemas?|protocols?|interfaces?)(/|[._-]|$)",
        )
        .expect("the sensitive path expression is valid")
    })
}

fn sensitive_change_regex() -> &'static regex::Regex {
    static REGEX: OnceLock<regex::Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        regex::Regex::new(
            r"(?i)(?:^|[^a-z0-9])(?:security|auth(?:entication|orization)?|permissions?|privacy|migrations?|deploy(?:ment)?|secrets?|tokens?|credentials?|passwords?|private[-_ ]?keys?|public[-_ ]?api|openapi|swagger|apis?|schemas?|protocols?|interfaces?|pub\s+(?:async\s+)?(?:fn|struct|enum|trait|type)|export\s+(?:default\s+)?(?:function|class|interface|type))(?:$|[^a-z0-9])",
        )
        .expect("the sensitive change expression is valid")
    })
}
