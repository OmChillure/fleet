//! Workspace-scoped fleet/orchestrator routes (cookie auth).
//!
//! Mapping: `workspace_id` == orchestrator `project_id`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use fleet_auth::{require_user, AppError, AppResult, AuthState};
use fleet_db::repos::workspaces as workspace_repo;
use fleet_orchestrator::domain::{OnTaskFailPolicy, PlanPolicy, PlanTask, WorkPlanBody, WorkPlanStatus};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/workspaces/{workspace_id}/fleet",
            get(get_fleet_project),
        )
        .route(
            "/api/workspaces/{workspace_id}/fleet/runs/fixture",
            post(post_fixture),
        )
        .route(
            "/api/workspaces/{workspace_id}/fleet/tick",
            post(post_tick),
        )
        .route(
            "/api/workspaces/{workspace_id}/fleet/tasks",
            get(get_tasks),
        )
        .route(
            "/api/workspaces/{workspace_id}/fleet/runs",
            get(get_runs),
        )
        .route(
            "/api/workspaces/{workspace_id}/fleet/sessions",
            get(get_sessions),
        )
        .route(
            "/api/workspaces/{workspace_id}/fleet/runs/{run_id}/cancel",
            post(post_cancel),
        )
}

async fn require_member(
    state: &AppState,
    jar: &CookieJar,
    workspace_id: Uuid,
) -> AppResult<(fleet_db::User, String)> {
    let auth = AuthState::from_app(state);
    let user = require_user(&auth, jar).await?;
    let role = workspace_repo::user_role(&state.db, workspace_id, user.id)
        .await?
        .ok_or_else(|| AppError::forbidden("not a member of this workspace"))?;
    Ok((user, role))
}

async fn ensure_project(state: &AppState, workspace_id: Uuid, name_hint: &str) -> AppResult<()> {
    state
        .orch
        .store
        .ensure_project(workspace_id, name_hint, 3)
        .await
        .map_err(|e| {
            tracing::error!(
                %workspace_id,
                name_hint,
                error = %e,
                "ensure_project failed"
            );
            AppError::internal(e.to_string())
        })?;
    Ok(())
}

/// Map store errors to API errors with full server-side logging.
fn map_orch_err(context: &str, e: impl std::fmt::Display) -> AppError {
    let msg = e.to_string();
    tracing::error!(context, error = %msg, "fleet route store error");
    AppError::internal(msg)
}

async fn get_fleet_project(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let (user, role) = require_member(&state, &jar, workspace_id).await?;
    let name = format!("{} fleet", user.email.split('@').next().unwrap_or("workspace"));
    ensure_project(&state, workspace_id, &name).await?;
    let project = state
        .orch
        .store
        .get_project(workspace_id)
        .await
        .map_err(|e| map_orch_err("get_project", e))?
        .ok_or_else(|| AppError::not_found("project"))?;
    let cfg = state
        .orch
        .store
        .get_project_config(workspace_id)
        .await
        .map_err(|e| map_orch_err("get_project_config", e))?;
    Ok(Json(json!({
        "project_id": project.id,
        "name": project.name,
        "workspace_id": workspace_id,
        "role": role,
        "sandbox_backend": state.orch.tick.runtime_backend(),
        "config": {
            "max_agents": cfg.max_agents,
            "sandbox_backend": cfg.sandbox_backend,
        }
    })))
}

#[derive(Debug, Deserialize)]
pub struct FixtureRequest {
    /// Optional; default is two-task chain.
    #[serde(default)]
    #[allow(dead_code)]
    pub kind: Option<String>,
}

async fn post_fixture(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Json(_body): Json<FixtureRequest>,
) -> AppResult<(StatusCode, Json<Value>)> {
    let (user, _) = require_member(&state, &jar, workspace_id).await?;
    let name = format!("{} fleet", user.email.split('@').next().unwrap_or("workspace"));
    ensure_project(&state, workspace_id, &name).await?;

    let run_id = Uuid::new_v4();
    let plan_id = Uuid::new_v4();
    let plan = WorkPlanBody {
        plan_id,
        project_id: workspace_id,
        run_id,
        version: 1,
        status: WorkPlanStatus::Accepted,
        source: "ui".into(),
        tasks: vec![
            PlanTask {
                plan_task_id: "t1".into(),
                title: "Root task".into(),
                summary: "Prepare workspace artifacts for the child task.".into(),
                spec_md: "# Root task\n\nWrite a short plan artifact, then succeed so `t2` can run.\n".into(),
                depends_on: vec![],
                worker_profile: "coding".into(),
                acceptance: vec!["artifact exists".into()],
                priority: 100,
                external_ref: None,
                timeout_s: Some(3600),
                max_retries: Some(1),
            },
            PlanTask {
                plan_task_id: "t2".into(),
                title: "Child task".into(),
                summary: "Consume root output and finish the chain.".into(),
                spec_md: "# Child task\n\nDepends on `t1`. Produce a final summary artifact.\n".into(),
                depends_on: vec!["t1".into()],
                worker_profile: "coding".into(),
                acceptance: vec!["chain complete".into()],
                priority: 50,
                external_ref: None,
                timeout_s: Some(3600),
                max_retries: None,
            },
        ],
        policy: PlanPolicy {
            max_agents_override: None,
            on_task_fail: OnTaskFailPolicy::FailRun,
            auto_sync_external: false,
        },
    };

    let (run, plan_row) = state
        .orch
        .store
        .insert_accepted_plan(workspace_id, plan)
        .await
        .map_err(|e| {
            tracing::error!(%workspace_id, error = %e, "insert_accepted_plan failed");
            AppError::bad_request(e.to_string())
        })?;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "run_id": run.id,
            "plan_id": plan_row.id,
            "status": run.status,
            "project_id": workspace_id,
        })),
    ))
}

async fn post_tick(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let (user, _) = require_member(&state, &jar, workspace_id).await?;
    let name = format!("{} fleet", user.email.split('@').next().unwrap_or("workspace"));
    ensure_project(&state, workspace_id, &name).await?;

    let report = state
        .orch
        .tick
        .tick_project(workspace_id)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("lock") {
                tracing::warn!(%workspace_id, error = %msg, "tick conflict (lock)");
                AppError::conflict(msg)
            } else {
                tracing::error!(%workspace_id, error = %msg, "tick failed");
                AppError::internal(msg)
            }
        })?;

    Ok(Json(json!({
        "tick_id": report.tick_id,
        "plans_materialized": report.plans_materialized,
        "tasks_materialized": report.tasks_materialized,
        "dep_updates": report.dep_updates,
        "inbox_applied": report.inbox_applied,
        "spawned": report.spawned,
        "spawn_failed": report.spawn_failed,
        "completed_via_runtime": report.completed_via_runtime,
        "killed": report.killed,
        "retried": report.retried,
        "cancel_tasks": report.cancel_tasks,
        "retry_promoted": report.retry_promoted,
        "reaped": report.reaped,
        "runs_terminated": report.runs_terminated,
    })))
}

async fn get_tasks(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    ensure_project(&state, workspace_id, "workspace fleet").await?;
    let tasks = state
        .orch
        .store
        .list_tasks(workspace_id, None, None)
        .await
        .map_err(|e| map_orch_err("list_tasks", e))?;
    Ok(Json(json!({
        "tasks": tasks.iter().map(|t| json!({
            "id": t.id,
            "run_id": t.run_id,
            "plan_task_id": t.plan_task_id,
            "title": t.title,
            "status": t.status,
            "priority": t.priority,
            "attempt": t.attempt,
            "progress_pct": t.progress_pct,
            "claimed_session_id": t.claimed_session_id,
            "worker_profile": t.worker_profile,
            "block_reason": t.block_reason,
            "failure_reason": t.failure_reason,
        })).collect::<Vec<_>>()
    })))
}

async fn get_sessions(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    ensure_project(&state, workspace_id, "workspace fleet").await?;
    let sessions = state
        .orch
        .store
        .list_sessions_for_project(workspace_id)
        .await
        .map_err(|e| map_orch_err("list_sessions", e))?;
    Ok(Json(json!({
        "sandbox_backend": state.orch.tick.runtime_backend(),
        "sessions": sessions.iter().map(|s| json!({
            "id": s.id,
            "run_id": s.run_id,
            "board_task_id": s.board_task_id,
            "attempt": s.attempt,
            "status": s.status,
            "runtime_backend": s.runtime_backend,
            "runtime_handle": s.runtime_handle,
            "worker_profile": s.worker_profile,
            "progress_pct": s.progress_pct,
            "last_heartbeat_at": s.last_heartbeat_at,
            "exit_reason": s.exit_reason,
            "work_dir": s.work_dir,
            "started_at": s.started_at,
            "exited_at": s.exited_at,
            "created_at": s.created_at,
        })).collect::<Vec<_>>()
    })))
}

async fn get_runs(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    ensure_project(&state, workspace_id, "workspace fleet").await?;
    let runs = state
        .orch
        .store
        .list_runs(workspace_id)
        .await
        .map_err(|e| map_orch_err("list_runs", e))?;
    Ok(Json(json!({
        "runs": runs.iter().map(|r| json!({
            "id": r.id,
            "status": r.status,
            "source": r.source,
            "cancel_requested_at": r.cancel_requested_at,
            "terminal_at": r.terminal_at,
            "created_at": r.created_at,
        })).collect::<Vec<_>>()
    })))
}

#[derive(Debug, Deserialize)]
pub struct CancelBody {
    #[allow(dead_code)]
    pub reason: Option<String>,
}

async fn post_cancel(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((workspace_id, run_id)): Path<(Uuid, Uuid)>,
    body: Option<Json<CancelBody>>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let _ = body;
    state
        .orch
        .store
        .request_cancel(workspace_id, run_id)
        .await
        .map_err(|e| {
            tracing::error!(%workspace_id, %run_id, error = %e, "request_cancel failed");
            AppError::bad_request(e.to_string())
        })?;
    Ok(Json(json!({ "run_id": run_id, "cancel_requested": true })))
}

trait AuthFromApp {
    fn from_app(state: &AppState) -> AuthState;
}

impl AuthFromApp for AuthState {
    fn from_app(state: &AppState) -> AuthState {
        AuthState {
            db: state.db.clone(),
            config: state.auth.clone(),
        }
    }
}
