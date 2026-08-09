//! Shared proxy request/result types (no secrets on public results).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Who initiated the proxy call (for audit logs only).
#[derive(Debug, Clone)]
pub enum ProxyCaller {
    Chat { user_id: Uuid },
    System,
}

impl ProxyCaller {
    pub fn kind(&self) -> &'static str {
        match self {
            ProxyCaller::Chat { .. } => "chat",
            ProxyCaller::System => "system",
        }
    }

    pub fn user_id(&self) -> Option<Uuid> {
        match self {
            ProxyCaller::Chat { user_id } => Some(*user_id),
            ProxyCaller::System => None,
        }
    }
}

/// Allowlisted integration proxy request.
#[derive(Debug, Clone)]
pub struct ProxyRequest {
    pub workspace_id: Uuid,
    pub provider: String,
    pub operation: String,
    pub args: Value,
    pub caller: ProxyCaller,
}

/// Sanitized proxy result safe for chat / agent context (no secrets).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyResult {
    pub ok: bool,
    pub summary: String,
    pub data: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    pub provider: String,
    pub operation: String,
}

impl ProxyResult {
    pub fn ok_result(
        provider: &str,
        operation: &str,
        summary: impl Into<String>,
        data: Value,
        status: Option<u16>,
    ) -> Self {
        Self {
            ok: true,
            summary: summary.into(),
            data,
            provider_status: status,
            error_code: None,
            provider: provider.to_string(),
            operation: operation.to_string(),
        }
    }

    pub fn err(
        provider: &str,
        operation: &str,
        code: &str,
        summary: impl Into<String>,
        status: Option<u16>,
    ) -> Self {
        Self {
            ok: false,
            summary: summary.into(),
            data: Value::Null,
            provider_status: status,
            error_code: Some(code.to_string()),
            provider: provider.to_string(),
            operation: operation.to_string(),
        }
    }
}

/// Max serialized JSON bytes returned in `ProxyResult.data`.
pub const MAX_PROXY_DATA_BYTES: usize = 64 * 1024;
