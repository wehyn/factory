use crate::{RedactedOutput, RunId};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductionStatus {
    Healthy,
    WaitingForDeployment,
    Failed,
    Unverified,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SmokeCheckState {
    Passed,
    Failed,
    Missing,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProductionObservation {
    pub id: Uuid,
    pub run_id: RunId,
    pub expected_sha: Option<String>,
    pub deployed_sha: Option<String>,
    pub smoke: SmokeCheckState,
    pub environment_id: Option<String>,
    pub status: ProductionStatus,
    pub detail: String,
    pub observed_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProductionAlert {
    pub id: Uuid,
    pub run_id: RunId,
    pub status: ProductionStatus,
    pub message: String,
    pub expected_sha: Option<String>,
    pub created_at_ms: i64,
    pub acknowledged_at_ms: Option<i64>,
    pub resolved_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProductionRunState {
    pub observation: Option<ProductionObservation>,
    pub alert: Option<ProductionAlert>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProductionAlertUpdate {
    pub state: ProductionRunState,
    pub new_alert: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductionInput {
    pub run_id: RunId,
    pub expected_sha: Option<String>,
    pub deployed_sha: Option<String>,
    pub smoke: SmokeCheckState,
    pub environment_id: Option<String>,
    pub detail: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct ProductionPolicy {
    pub environment_id: Option<String>,
    pub identity_url: Option<String>,
    pub identity_json_field: String,
    pub smoke_url: Option<String>,
    pub timeout_seconds: u64,
}

impl Default for ProductionPolicy {
    fn default() -> Self {
        Self {
            environment_id: None,
            identity_url: None,
            identity_json_field: "commit_sha".to_owned(),
            smoke_url: None,
            timeout_seconds: 10,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProductionObserver {
    curl_executable: PathBuf,
}

impl Default for ProductionObserver {
    fn default() -> Self {
        Self::new()
    }
}

impl ProductionObserver {
    pub fn new() -> Self {
        Self {
            curl_executable: PathBuf::from("curl"),
        }
    }

    pub fn with_executable(executable: impl Into<PathBuf>) -> Self {
        Self {
            curl_executable: executable.into(),
        }
    }

    /// Performs exactly one read-only identity request and one smoke request. It never retries,
    /// rolls back, or changes a deployment.
    pub fn observe(
        &self,
        run_id: RunId,
        expected_sha: Option<&str>,
        policy: &ProductionPolicy,
    ) -> ProductionObservation {
        let mut details = Vec::new();
        let expected_sha = expected_sha.map(str::to_owned);
        if !expected_sha.as_deref().is_some_and(is_sha) {
            details.push("A merged commit SHA is not available.".to_owned());
            return evaluate_production(ProductionInput {
                run_id,
                expected_sha,
                deployed_sha: None,
                smoke: SmokeCheckState::Missing,
                environment_id: policy.environment_id.clone(),
                detail: details.join(" "),
            });
        }

        if let Err(error) = validate_production_policy(policy) {
            return evaluate_production(ProductionInput {
                run_id,
                expected_sha,
                deployed_sha: None,
                smoke: SmokeCheckState::Missing,
                environment_id: policy.environment_id.clone(),
                detail: format!("Production policy is invalid: {}", safe_detail(error)),
            });
        }

        let deployed_sha = match policy.identity_url.as_deref() {
            Some(url) => match self.read_deployment_sha(
                url,
                &policy.identity_json_field,
                policy.timeout_seconds,
            ) {
                Ok(sha) => Some(sha),
                Err(error) => {
                    details.push(format!(
                        "Deployment identity could not be read: {}",
                        safe_detail(error)
                    ));
                    None
                }
            },
            None => {
                details.push("Deployment identity URL is not configured.".to_owned());
                None
            }
        };
        let smoke = match policy.smoke_url.as_deref() {
            Some(url) => match self.read_smoke(url, policy.timeout_seconds) {
                Ok(()) => SmokeCheckState::Passed,
                Err(error) => {
                    details.push(format!(
                        "Production smoke check failed: {}",
                        safe_detail(error)
                    ));
                    SmokeCheckState::Failed
                }
            },
            None => {
                details.push("Production smoke URL is not configured.".to_owned());
                SmokeCheckState::Missing
            }
        };

        if let (Some(expected), Some(deployed)) = (&expected_sha, &deployed_sha) {
            if expected != deployed {
                details.push("The deployed commit does not match the merged commit.".to_owned());
            }
        }
        if smoke == SmokeCheckState::Passed && deployed_sha.as_deref() == expected_sha.as_deref() {
            details.push(
                "Deployment commit matches and the production smoke check passed.".to_owned(),
            );
        }
        evaluate_production(ProductionInput {
            run_id,
            expected_sha,
            deployed_sha,
            smoke,
            environment_id: policy.environment_id.clone(),
            detail: details.join(" "),
        })
    }

    fn read_deployment_sha(&self, url: &str, field: &str, timeout_seconds: u64) -> Result<String> {
        let output = Command::new(&self.curl_executable)
            .args([
                "--fail",
                "--silent",
                "--show-error",
                "--max-time",
                &timeout_seconds.to_string(),
                "--max-filesize",
                "65536",
                url,
            ])
            .output()
            .with_context(|| {
                format!(
                    "starting production identity probe at {}",
                    self.curl_executable.display()
                )
            })?;
        anyhow::ensure!(
            output.status.success(),
            "identity endpoint returned an error"
        );
        let value: Value = serde_json::from_slice(&output.stdout)
            .context("deployment identity response was not valid JSON")?;
        let mut value = &value;
        for part in field.split('.') {
            value = value
                .get(part)
                .ok_or_else(|| anyhow::anyhow!("deployment identity field is missing"))?;
        }
        let sha = value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("deployment identity field is not a string"))?;
        anyhow::ensure!(is_sha(sha), "deployment identity is not a full commit SHA");
        Ok(sha.to_ascii_lowercase())
    }

    fn read_smoke(&self, url: &str, timeout_seconds: u64) -> Result<()> {
        let timeout = timeout_seconds.to_string();
        let output = Command::new(&self.curl_executable)
            .args([
                "--silent",
                "--show-error",
                "--max-time",
                &timeout,
                "--output",
                "/dev/null",
                "--write-out",
                "%{http_code}",
                url,
            ])
            .output()
            .with_context(|| {
                format!(
                    "starting production smoke probe at {}",
                    self.curl_executable.display()
                )
            })?;
        anyhow::ensure!(
            output.status.success(),
            "smoke endpoint could not be reached"
        );
        let code = String::from_utf8_lossy(&output.stdout);
        let code = code
            .trim()
            .parse::<u16>()
            .context("smoke endpoint returned an invalid HTTP status")?;
        anyhow::ensure!(
            (200..300).contains(&code),
            "smoke endpoint returned HTTP {code}"
        );
        Ok(())
    }
}

pub fn evaluate_production(input: ProductionInput) -> ProductionObservation {
    let status = if !input.expected_sha.as_deref().is_some_and(is_sha) {
        ProductionStatus::Unverified
    } else if input.deployed_sha.as_deref().is_none_or(|sha| !is_sha(sha)) {
        ProductionStatus::Unverified
    } else if input.expected_sha.as_deref() != input.deployed_sha.as_deref() {
        ProductionStatus::WaitingForDeployment
    } else {
        match input.smoke {
            SmokeCheckState::Passed => ProductionStatus::Healthy,
            SmokeCheckState::Failed => ProductionStatus::Failed,
            SmokeCheckState::Missing => ProductionStatus::Unverified,
        }
    };
    ProductionObservation {
        id: Uuid::new_v4(),
        run_id: input.run_id,
        expected_sha: input.expected_sha,
        deployed_sha: input.deployed_sha,
        smoke: input.smoke,
        environment_id: input.environment_id,
        status,
        detail: RedactedOutput::new(input.detail).as_str().to_owned(),
        observed_at_ms: now_ms(),
    }
}

pub fn load_production_policy(repository_root: &Path) -> Result<ProductionPolicy> {
    let path = repository_root.join(".agentic-factory.json");
    if !path.exists() {
        let policy = ProductionPolicy::default();
        validate_production_policy(&policy)?;
        return Ok(policy);
    }
    let bytes = std::fs::read(&path)
        .with_context(|| format!("reading repository config at {}", path.display()))?;
    let config: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("decoding repository config at {}", path.display()))?;
    anyhow::ensure!(
        config.is_object(),
        "repository config must be a JSON object"
    );
    let production = config.get("production").cloned().unwrap_or(Value::Null);
    let policy = if production.is_null() {
        ProductionPolicy::default()
    } else {
        serde_json::from_value::<ProductionPolicy>(production)
            .with_context(|| format!("decoding production policy at {}", path.display()))?
    };
    validate_production_policy(&policy)?;
    Ok(policy)
}

fn validate_production_policy(policy: &ProductionPolicy) -> Result<()> {
    let environment_id = policy.environment_id.as_deref().unwrap_or_default();
    anyhow::ensure!(
        !environment_id.is_empty()
            && environment_id.len() <= 64
            && environment_id
                .bytes()
                .all(|byte| { byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') }),
        "production environment_id must be 1 to 64 ASCII letters, digits, or -_. characters"
    );
    anyhow::ensure!(
        (1..=60).contains(&policy.timeout_seconds),
        "production timeout_seconds must be between 1 and 60"
    );
    anyhow::ensure!(
        !policy.identity_json_field.is_empty()
            && policy.identity_json_field.split('.').all(|part| {
                !part.is_empty()
                    && part.chars().all(|character| {
                        character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
                    })
            }),
        "production identity_json_field must be a dotted JSON field path"
    );
    for url in [policy.identity_url.as_deref(), policy.smoke_url.as_deref()]
        .into_iter()
        .flatten()
    {
        let authority = url
            .strip_prefix("https://")
            .unwrap_or_default()
            .split('/')
            .next()
            .unwrap_or_default();
        anyhow::ensure!(
            url.starts_with("https://")
                && !authority.is_empty()
                && !authority.contains('@')
                && !url.chars().any(char::is_whitespace),
            "production URLs must use HTTPS without embedded credentials"
        );
    }
    Ok(())
}

fn safe_detail(error: impl std::fmt::Display) -> String {
    RedactedOutput::new(error.to_string()).as_str().to_owned()
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
