use super::status::WorkPlanStatus;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_TASKS_PER_PLAN: usize = 1000;
pub const MAX_PLAN_BODY_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_SPEC_MD_BYTES: usize = 64 * 1024;
pub const MAX_DEPENDENCY_FAN_IN: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnTaskFailPolicy {
    FailRun,
    Leave,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalRef {
    pub provider: String,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanTask {
    pub plan_task_id: String,
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub spec_md: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub worker_profile: String,
    #[serde(default)]
    pub acceptance: Vec<String>,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub external_ref: Option<ExternalRef>,
    #[serde(default)]
    pub timeout_s: Option<i32>,
    #[serde(default)]
    pub max_retries: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanPolicy {
    #[serde(default)]
    pub max_agents_override: Option<i32>,
    #[serde(default = "default_on_task_fail")]
    pub on_task_fail: OnTaskFailPolicy,
    #[serde(default)]
    pub auto_sync_external: bool,
    /// Chat composer provider (`grok`, `ollama`, later `claude`). Workers use this.
    #[serde(default)]
    pub llm_provider: Option<String>,
    /// Exact model id selected in chat (not a random default).
    #[serde(default)]
    pub llm_model: Option<String>,
}

fn default_on_task_fail() -> OnTaskFailPolicy {
    OnTaskFailPolicy::FailRun
}

impl Default for PlanPolicy {
    fn default() -> Self {
        Self {
            max_agents_override: None,
            on_task_fail: OnTaskFailPolicy::FailRun,
            auto_sync_external: false,
            llm_provider: None,
            llm_model: None,
        }
    }
}

/// Immutable accepted plan document (handoff artifact).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkPlanBody {
    pub plan_id: Uuid,
    pub project_id: Uuid,
    pub run_id: Uuid,
    pub version: i32,
    pub status: WorkPlanStatus,
    pub source: String,
    pub tasks: Vec<PlanTask>,
    #[serde(default)]
    pub policy: PlanPolicy,
}
