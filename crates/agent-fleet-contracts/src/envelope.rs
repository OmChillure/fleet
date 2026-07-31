//! Common envelope fields present on every bus message.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{MessageId, SandboxId, SessionId, TaskId, TurnId, WorkflowId};

/// Semantic version of the payload schema for this crate release line.
pub const SCHEMA_VERSION: &str = "0.1.0";

/// Who produced the message.
///
/// Wire form is a string: `orchestrator`, `sandbox-manager`, `driver`,
/// `integration:<name>`, or any other raw label preserved as [`Producer::Other`]
/// (forward-compat — never rewritten with a prefix).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Producer {
    Orchestrator,
    SandboxManager,
    Driver,
    /// Named integration adapter, e.g. `slack` → wire `integration:slack`.
    Integration(String),
    /// Unknown / future producer label, preserved verbatim on the wire.
    Other(String),
}

impl Producer {
    /// Wire representation of this producer.
    pub fn to_wire(&self) -> String {
        match self {
            Self::Orchestrator => "orchestrator".into(),
            Self::SandboxManager => "sandbox-manager".into(),
            Self::Driver => "driver".into(),
            Self::Integration(name) => format!("integration:{name}"),
            Self::Other(raw) => raw.clone(),
        }
    }
}

impl std::fmt::Display for Producer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Orchestrator => f.write_str("orchestrator"),
            Self::SandboxManager => f.write_str("sandbox-manager"),
            Self::Driver => f.write_str("driver"),
            Self::Integration(name) => write!(f, "integration:{name}"),
            Self::Other(raw) => f.write_str(raw),
        }
    }
}

impl Serialize for Producer {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_wire())
    }
}

impl<'de> Deserialize<'de> for Producer {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "orchestrator" => Self::Orchestrator,
            "sandbox-manager" => Self::SandboxManager,
            "driver" => Self::Driver,
            other => {
                if let Some(name) = other.strip_prefix("integration:") {
                    Self::Integration(name.to_string())
                } else {
                    Self::Other(other.to_string())
                }
            }
        })
    }
}

/// Envelope metadata shared by every command and event.
///
/// On the wire this is flattened next to `type` + `payload` (see [`crate::BusMessage`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub message_id: MessageId,
    pub ts: DateTime<Utc>,
    pub producer: Producer,
    pub schema_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_id: Option<SandboxId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<WorkflowId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    pub correlation_id: MessageId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<MessageId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
}

impl Envelope {
    /// Fresh envelope with a new `message_id`, `ts = now`, and
    /// `schema_version = SCHEMA_VERSION`. `correlation_id` defaults to `message_id`.
    pub fn new(producer: Producer) -> Self {
        let message_id = MessageId::new();
        Self {
            message_id,
            ts: Utc::now(),
            producer,
            schema_version: SCHEMA_VERSION.to_string(),
            session_id: None,
            sandbox_id: None,
            task_id: None,
            workflow_id: None,
            turn_id: None,
            correlation_id: message_id,
            causation_id: None,
            tenant_id: None,
        }
    }

    pub fn with_correlation(mut self, correlation_id: MessageId) -> Self {
        self.correlation_id = correlation_id;
        self
    }

    pub fn with_causation(mut self, causation_id: MessageId) -> Self {
        self.causation_id = Some(causation_id);
        self
    }

    pub fn with_session(mut self, session_id: SessionId) -> Self {
        self.session_id = Some(session_id);
        self
    }

    pub fn with_sandbox(mut self, sandbox_id: SandboxId) -> Self {
        self.sandbox_id = Some(sandbox_id);
        self
    }

    pub fn with_task(mut self, task_id: TaskId) -> Self {
        self.task_id = Some(task_id);
        self
    }

    pub fn with_workflow(mut self, workflow_id: WorkflowId) -> Self {
        self.workflow_id = Some(workflow_id);
        self
    }

    pub fn with_turn(mut self, turn_id: TurnId) -> Self {
        self.turn_id = Some(turn_id);
        self
    }

    pub fn with_tenant(mut self, tenant_id: impl Into<String>) -> Self {
        self.tenant_id = Some(tenant_id.into());
        self
    }

    /// Fill empty scope fields from a payload-derived scope (does not overwrite set fields).
    pub fn merge_scope(&mut self, scope: MessageScope) {
        if self.session_id.is_none() {
            self.session_id = scope.session_id;
        }
        if self.sandbox_id.is_none() {
            self.sandbox_id = scope.sandbox_id;
        }
        if self.task_id.is_none() {
            self.task_id = scope.task_id;
        }
        if self.workflow_id.is_none() {
            self.workflow_id = scope.workflow_id;
        }
        if self.turn_id.is_none() {
            self.turn_id = scope.turn_id;
        }
    }
}

/// Scope identifiers commonly carried on payloads and mirrored on the envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MessageScope {
    pub session_id: Option<SessionId>,
    pub sandbox_id: Option<SandboxId>,
    pub task_id: Option<TaskId>,
    pub workflow_id: Option<WorkflowId>,
    pub turn_id: Option<TurnId>,
}

impl MessageScope {
    pub fn session(session_id: SessionId) -> Self {
        Self {
            session_id: Some(session_id),
            ..Default::default()
        }
    }

    pub fn sandbox(sandbox_id: SandboxId) -> Self {
        Self {
            sandbox_id: Some(sandbox_id),
            ..Default::default()
        }
    }
}
