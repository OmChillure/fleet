use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct ProjectRow {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct ProjectConfigRow {
    pub project_id: Uuid,
    pub max_agents: i32,
    pub tick_interval_s: i32,
    pub heartbeat_stale_s: i32,
    pub spawn_timeout_s: i32,
    pub kill_grace_s: i32,
    pub default_task_timeout_s: i32,
    pub default_max_retries: i32,
    pub retry_backoff_s: i32,
    pub on_task_fail: String,
    pub auto_sync_external: bool,
    pub sandbox_backend: String,
    /// Clone URL for the project's single git remote. None = no clone.
    pub repo_url: Option<String>,
    /// Branch, tag, or commit checked out after clone. Default `main`.
    pub git_ref: String,
    /// `off` | `ollama` — Grok is env-only, never stored here.
    pub fleet_llm: String,
    pub fleet_llm_model: String,
    pub updated_at: DateTime<Utc>,
}

impl ProjectConfigRow {
    pub fn public_json(&self) -> serde_json::Value {
        serde_json::json!({
            "project_id": self.project_id,
            "max_agents": self.max_agents,
            "tick_interval_s": self.tick_interval_s,
            "heartbeat_stale_s": self.heartbeat_stale_s,
            "spawn_timeout_s": self.spawn_timeout_s,
            "kill_grace_s": self.kill_grace_s,
            "default_task_timeout_s": self.default_task_timeout_s,
            "default_max_retries": self.default_max_retries,
            "retry_backoff_s": self.retry_backoff_s,
            "on_task_fail": self.on_task_fail,
            "auto_sync_external": self.auto_sync_external,
            "sandbox_backend": self.sandbox_backend,
            "repo_url": self.repo_url,
            "git_ref": self.git_ref,
            "fleet_llm": self.fleet_llm,
            "fleet_llm_model": self.fleet_llm_model,
        })
    }

    /// Binding used at spawn time. None when the project has no remote yet.
    pub fn git_remote(&self) -> Option<crate::domain::ProjectGitRemote> {
        let url = self.repo_url.as_deref()?.trim();
        if url.is_empty() {
            return None;
        }
        Some(crate::domain::ProjectGitRemote {
            url: url.to_string(),
            git_ref: if self.git_ref.trim().is_empty() {
                "main".into()
            } else {
                self.git_ref.clone()
            },
        })
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct WorkRunRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub status: String,
    pub source: String,
    pub cancel_requested_at: Option<DateTime<Utc>>,
    pub orchestration_owned_at: Option<DateTime<Utc>>,
    pub first_spawn_at: Option<DateTime<Utc>>,
    pub terminal_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct WorkPlanRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub run_id: Uuid,
    pub version: i32,
    pub status: String,
    pub source: String,
    pub body: Value,
    pub accepted_at: Option<DateTime<Utc>>,
    pub materialized_at: Option<DateTime<Utc>>,
    pub in_force_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct BoardTaskRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub run_id: Uuid,
    pub plan_id: Uuid,
    pub plan_task_id: String,
    pub title: String,
    pub summary: String,
    pub spec_md: String,
    pub status: String,
    pub depends_on: Vec<Uuid>,
    pub priority: i32,
    pub worker_profile: String,
    pub acceptance: Value,
    pub external_ref: Option<Value>,
    pub timeout_s: Option<i32>,
    pub max_retries: i32,
    pub attempt: i32,
    pub claimed_session_id: Option<Uuid>,
    pub last_report_seq: i32,
    pub progress_pct: i32,
    pub block_reason: Option<String>,
    pub hitl_notes: Option<String>,
    pub hitl_artifacts: Option<Value>,
    pub hitl_resume_requested_at: Option<DateTime<Utc>>,
    pub hitl_notified_at: Option<DateTime<Utc>>,
    pub hitl_resolved_at: Option<DateTime<Utc>>,
    pub failure_reason: Option<String>,
    pub artifacts: Option<Value>,
    pub retry_not_before: Option<DateTime<Utc>>,
    pub assigned_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub terminal_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct WorkerSessionRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub run_id: Uuid,
    pub board_task_id: Uuid,
    pub attempt: i32,
    pub status: String,
    pub runtime_backend: String,
    pub runtime_handle: Option<String>,
    pub worker_token_hash: String,
    pub worker_profile: String,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    pub progress_pct: i32,
    pub exit_reason: Option<String>,
    pub kill_grace_deadline: Option<DateTime<Utc>>,
    pub work_dir: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub exited_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct WorkerReportRow {
    pub id: i64,
    pub project_id: Uuid,
    pub session_id: Uuid,
    pub board_task_id: Uuid,
    pub seq: i32,
    pub kind: String,
    pub payload: Value,
    pub received_at: DateTime<Utc>,
    pub applied_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct BoardSnapshot {
    pub tasks: Vec<BoardTaskRow>,
    pub sessions: Vec<WorkerSessionRow>,
}

#[derive(Debug, Clone)]
pub struct NewWorkerSession {
    pub id: Uuid,
    pub project_id: Uuid,
    pub run_id: Uuid,
    pub board_task_id: Uuid,
    pub attempt: i32,
    pub runtime_backend: String,
    pub worker_token: String,
    pub worker_profile: String,
    pub work_dir: Option<String>,
}
