use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{
    fmt,
    sync::OnceLock,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const MAX_OUTPUT_CHARS: usize = 16_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct SessionId(pub Uuid);

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RedactedOutput(String);

impl RedactedOutput {
    pub fn new(raw: impl AsRef<str>) -> Self {
        let mut value = credential_regex()
            .replace_all(raw.as_ref(), "$1[REDACTED]")
            .into_owned();
        value = bearer_regex()
            .replace_all(&value, "$1 [REDACTED]")
            .into_owned();
        value = token_regex().replace_all(&value, "[REDACTED]").into_owned();

        let mut chars = value.chars();
        let bounded: String = chars.by_ref().take(MAX_OUTPUT_CHARS).collect();
        if chars.next().is_some() {
            Self(format!("{bounded}\n[output truncated]"))
        } else {
            Self(bounded)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for RedactedOutput {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RedactedOutput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Ok(Self::new(raw))
    }
}

fn credential_regex() -> &'static regex::Regex {
    static REGEX: OnceLock<regex::Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        regex::Regex::new(
            r#"(?i)(["']?(?:[A-Z0-9]+[_-])*(?:authorization|proxy-authorization|api[_-]?key|access[_-]?token|refresh[_-]?token|id[_-]?token|client[_-]?secret|secret|password|passwd|token|cookie|private[_-]?key|credentials)(?:[_-][A-Z0-9]+)*["']?\s*[:=]\s*)(?:"(?:\\.|[^"\\])*"|'[^'\r\n]*'|(?:bearer|basic)\s+[^\s,;}\]]+|[^\s,;}\]]+)"#,
        )
        .expect("the credential-redaction expression is valid")
    })
}

fn bearer_regex() -> &'static regex::Regex {
    static REGEX: OnceLock<regex::Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        regex::Regex::new(r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/-]+=*")
            .expect("the bearer-redaction expression is valid")
    })
}

fn token_regex() -> &'static regex::Regex {
    static REGEX: OnceLock<regex::Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        regex::Regex::new(
            r"(?i)\b(?:sk-[A-Za-z0-9_-]{16,}|gh[pousr]_[A-Za-z0-9_]{20,}|github_pat_[A-Za-z0-9_]{20,}|xox[baprs]-[A-Za-z0-9-]{10,})\b",
        )
        .expect("the common-token-redaction expression is valid")
    })
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum EventKind {
    SessionCreated,
    SessionStarted { thread_id: String },
    TurnStarted { turn_id: String },
    Output(RedactedOutput),
    TurnCompleted,
    SessionInterrupted,
    SessionFailed { message: RedactedOutput },
}

impl EventKind {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::SessionCreated => "session_created",
            Self::SessionStarted { .. } => "session_started",
            Self::TurnStarted { .. } => "turn_started",
            Self::Output(_) => "output",
            Self::TurnCompleted => "turn_completed",
            Self::SessionInterrupted => "session_interrupted",
            Self::SessionFailed { .. } => "session_failed",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Event {
    pub id: Uuid,
    pub session_id: SessionId,
    pub kind: EventKind,
    pub created_at_ms: i64,
}

impl Event {
    pub fn new(session_id: SessionId, kind: EventKind) -> Self {
        let created_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(i64::MAX as u128) as i64;

        Self {
            id: Uuid::new_v4(),
            session_id,
            kind,
            created_at_ms,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionProcessState {
    Starting,
    Running,
    Completed,
    Interrupted,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionSnapshot {
    pub session_id: SessionId,
    pub process_state: SessionProcessState,
    pub thread_id: Option<String>,
    pub output: Vec<String>,
    pub failure_count: u32,
    pub last_sequence: i64,
    pub created_at_ms: i64,
}

impl SessionSnapshot {
    pub(crate) fn new(session_id: SessionId, created_at_ms: i64, sequence: i64) -> Self {
        Self {
            session_id,
            process_state: SessionProcessState::Starting,
            thread_id: None,
            output: Vec::new(),
            failure_count: 0,
            last_sequence: sequence,
            created_at_ms,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FactorySnapshot {
    pub last_sequence: i64,
    pub sessions: Vec<SessionSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SequencedEvent {
    pub sequence: i64,
    pub event: Event,
}
