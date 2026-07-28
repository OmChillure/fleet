//! Agent / ACP-facing events (emitted by the Driver).

use serde::{Deserialize, Serialize};

use crate::ids::{SessionId, ToolCallId, TurnId};
use crate::types::{Redaction, SideEffectClass};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTurnStarted {
    pub session_id: SessionId,
    pub turn_id: TurnId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentEvent {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    /// Monotonic per session for trace reconstruction.
    pub seq: u64,
    /// ACP method name or logical kind (`message_delta`, `thought`, …).
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acp_method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    pub redaction: Redaction,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallProposed {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    /// May be redacted in stored form.
    pub arguments: serde_json::Value,
    pub side_effect_class: SideEffectClass,
    pub requires_secret: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_policy_action: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentPermissionRequired {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallFinished {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTurnCompleted {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTurnFailed {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
}
