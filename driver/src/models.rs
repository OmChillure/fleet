use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessKind {
    Codex,
    Opencode,
    Grok,
    Custom,
}

impl HarnessKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Opencode => "opencode",
            Self::Grok => "grok",
            Self::Custom => "custom",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "codex" => Self::Codex,
            "opencode" | "open_code" => Self::Opencode,
            "grok" | "grok_build" => Self::Grok,
            _ => Self::Custom,
        }
    }

    pub fn all() -> &'static [HarnessKind] {
        &[Self::Codex, Self::Opencode, Self::Grok, Self::Custom]
    }
}

impl std::fmt::Display for HarnessKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "running" => Self::Running,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => Self::Pending,
        }
    }
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
    pub notes: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillRef {
    /// Absolute path or name of a skill directory / SKILL.md
    pub path: String,
    #[serde(default)]
    pub enabled: bool,
}

impl Default for SkillRef {
    fn default() -> Self {
        Self {
            path: String::new(),
            enabled: true,
        }
    }
}


#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigChoice {
    pub value: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Session config options discovered from an ACP agent (`session/new` → configOptions).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiscoveredConfig {
    #[serde(default)]
    pub models: Vec<ConfigChoice>,
    #[serde(default)]
    pub thought_levels: Vec<ConfigChoice>,
    #[serde(default)]
    pub modes: Vec<ConfigChoice>,
    /// config option id for model (usually "model")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_config_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_config_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode_config_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessProfile {
    pub id: String,
    pub name: String,
    pub harness: HarnessKind,
    pub command: String,
    pub args: Vec<String>,
    pub model: Option<String>,
    /// Reasoning / thought level (ACP category thought_level), if agent supports it.
    #[serde(default)]
    pub thought_level: Option<String>,
    /// Optional base system / preamble prompt prepended to every task
    pub system_prompt: String,
    pub skills: Vec<SkillRef>,
    pub mcp_servers: Vec<McpServerConfig>,
    pub env: HashMap<String, String>,
    pub auto_approve: bool,
    /// Last ACP-discovered models / thought levels / modes for this profile.
    #[serde(default)]
    pub discovered: DiscoveredConfig,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub project_id: String,
    pub profile_id: String,
    pub title: String,
    pub prompt: String,
    pub status: TaskStatus,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRun {
    pub id: String,
    pub task_id: String,
    pub status: TaskStatus,
    pub session_id: Option<String>,
    pub log: String,
    pub error: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TaskView {
    pub task: Task,
    pub project_name: String,
    pub project_path: String,
    pub profile_name: String,
    pub harness: HarnessKind,
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn default_db_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("agent-fleet")
        .join("fleet.db")
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_kind_parse_and_display() {
        assert_eq!(HarnessKind::parse("codex"), HarnessKind::Codex);
        assert_eq!(HarnessKind::parse("OpenCode"), HarnessKind::Opencode);
        assert_eq!(HarnessKind::parse("grok_build"), HarnessKind::Grok);
        assert_eq!(HarnessKind::parse("other"), HarnessKind::Custom);
        assert_eq!(HarnessKind::Codex.as_str(), "codex");
        assert_eq!(format!("{}", HarnessKind::Grok), "grok");
        assert_eq!(HarnessKind::all().len(), 4);
    }

    #[test]
    fn task_status_parse() {
        assert_eq!(TaskStatus::parse("running"), TaskStatus::Running);
        assert_eq!(TaskStatus::parse("done"), TaskStatus::Done);
        assert_eq!(TaskStatus::parse("failed"), TaskStatus::Failed);
        assert_eq!(TaskStatus::parse("cancelled"), TaskStatus::Cancelled);
        assert_eq!(TaskStatus::parse("nope"), TaskStatus::Pending);
        assert_eq!(TaskStatus::Done.as_str(), "done");
    }

    #[test]
    fn new_id_and_default_db_path() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
        assert!(uuid::Uuid::parse_str(&a).is_ok());
        let path = default_db_path();
        assert!(path.to_string_lossy().contains("agent-fleet"));
        assert!(path.extension().and_then(|s| s.to_str()) == Some("db") || path.ends_with("fleet.db"));
    }
}
