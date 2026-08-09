use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::auth::{AdminAuth, WorkerAuth};
use super::state::AppState;
use crate::domain::WorkPlanBody;
use crate::store::StoreError;

type ApiError = (StatusCode, Json<Value>);

fn err(status: StatusCode, msg: impl Into<String>) -> ApiError {
    let msg = msg.into();
    if status.is_server_error() {
        tracing::error!(status = status.as_u16(), error = %msg, "orchestrator api error");
    } else {
        tracing::warn!(status = status.as_u16(), error = %msg, "orchestrator api error");
    }
    (status, Json(json!({ "error": msg })))
}

fn map_store(e: StoreError) -> ApiError {
    match e {
        StoreError::NotFound(m) => err(StatusCode::NOT_FOUND, m),
        StoreError::Validation(m) => err(StatusCode::BAD_REQUEST, m),
        StoreError::Conflict(m) => err(StatusCode::CONFLICT, m),
        StoreError::Sqlx(e) => {
            // Log the full sqlx detail once; client sees a generic message.
            tracing::error!(error = %e, "store sqlx error");
            err(StatusCode::INTERNAL_SERVER_ERROR, "database error")
        }
    }
}

pub async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

/// OpenAPI 3 document (PR6 follow-up).
pub async fn openapi_yaml() -> impl axum::response::IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "application/yaml; charset=utf-8",
        )],
        include_str!("../../api/openapi.yaml"),
    )
}

/// Standalone binary root (when not mounted under fleet-server).
pub async fn root_index() -> Json<Value> {
    Json(json!({
        "service": "fleet-orchestrator",
        "status": "ok",
        "message": "Orchestrator control plane",
        "links": {
            "healthz": "/healthz",
            "readyz": "/readyz",
            "openapi": "/openapi.yaml",
            "projects": "POST /v1/projects",
        }
    }))
}

pub async fn readyz(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    state
        .store
        .pool()
        .acquire()
        .await
        .map_err(|_| err(StatusCode::SERVICE_UNAVAILABLE, "db unavailable"))?;
    Ok(Json(json!({ "status": "ready" })))
}

#[derive(Debug, Deserialize)]
pub struct CreateProjectBody {
    pub name: String,
    pub max_agents: Option<i32>,
    pub repo_url: Option<String>,
    pub git_ref: Option<String>,
}

pub async fn create_project(
    State(state): State<AppState>,
    _admin: AdminAuth,
    Json(body): Json<CreateProjectBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if body.name.trim().is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "name required"));
    }
    let max = body.max_agents.unwrap_or(3).max(0);
    let remote = crate::domain::parse_project_git_remote(
        body.repo_url.as_deref(),
        body.git_ref.as_deref(),
    )
    .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
    let project = state
        .store
        .create_project(body.name.trim(), max)
        .await
        .map_err(map_store)?;
    let cfg = if let Some(remote) = remote.as_ref() {
        state
            .store
            .set_project_git_remote(project.id, Some(remote))
            .await
            .map_err(map_store)?
    } else {
        state
            .store
            .get_project_config(project.id)
            .await
            .map_err(map_store)?
    };
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": project.id,
            "name": project.name,
            "created_at": project.created_at,
            "config": cfg.public_json(),
        })),
    ))
}

pub async fn get_project(
    State(state): State<AppState>,
    _admin: AdminAuth,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let p = state
        .store
        .get_project(project_id)
        .await
        .map_err(map_store)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "project not found"))?;
    Ok(Json(json!({
        "id": p.id,
        "name": p.name,
        "created_at": p.created_at,
    })))
}

pub async fn get_config(
    State(state): State<AppState>,
    _admin: AdminAuth,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let cfg = state
        .store
        .get_project_config(project_id)
        .await
        .map_err(map_store)?;
    Ok(Json(cfg.public_json()))
}

#[derive(Debug, Deserialize)]
pub struct PatchConfigBody {
    pub repo_url: Option<String>,
    pub git_ref: Option<String>,
}

pub async fn patch_config(
    State(state): State<AppState>,
    _admin: AdminAuth,
    Path(project_id): Path<Uuid>,
    Json(body): Json<PatchConfigBody>,
) -> Result<Json<Value>, ApiError> {
    // Keep existing ref when only repo_url is sent.
    let existing = state
        .store
        .get_project_config(project_id)
        .await
        .map_err(map_store)?;
    let git_ref = body.git_ref.as_deref().or(Some(existing.git_ref.as_str()));
    let remote = crate::domain::parse_project_git_remote(body.repo_url.as_deref(), git_ref)
        .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
    // If the client omitted repo_url, keep the current URL and only change ref.
    let remote = match body.repo_url {
        None => match existing.git_remote() {
            Some(mut r) => {
                if let Some(rf) = body.git_ref.as_deref() {
                    r.git_ref = crate::domain::normalize_git_ref(Some(rf))
                        .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
                }
                Some(r)
            }
            None => {
                if body.git_ref.is_some() {
                    return Err(err(
                        StatusCode::BAD_REQUEST,
                        "git_ref requires repo_url on this project",
                    ));
                }
                None
            }
        },
        Some(_) => remote,
    };
    let cfg = state
        .store
        .set_project_git_remote(project_id, remote.as_ref())
        .await
        .map_err(map_store)?;
    Ok(Json(cfg.public_json()))
}

#[derive(Debug, Deserialize)]
pub struct FixtureBody {
    pub plan: WorkPlanBody,
}

pub async fn fixture_run(
    State(state): State<AppState>,
    _admin: AdminAuth,
    Path(project_id): Path<Uuid>,
    Json(body): Json<FixtureBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    // Ensure project exists
    state
        .store
        .get_project(project_id)
        .await
        .map_err(map_store)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "project not found"))?;

    let mut plan = body.plan;
    plan.project_id = project_id;
    if plan.run_id == Uuid::nil() {
        plan.run_id = Uuid::new_v4();
    }
    if plan.plan_id == Uuid::nil() {
        plan.plan_id = Uuid::new_v4();
    }
    let (run, plan_row) = state
        .store
        .insert_accepted_plan(project_id, plan)
        .await
        .map_err(map_store)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "run_id": run.id,
            "plan_id": plan_row.id,
            "status": run.status,
        })),
    ))
}

pub async fn list_runs(
    State(state): State<AppState>,
    _admin: AdminAuth,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let runs = state
        .store
        .list_runs(project_id)
        .await
        .map_err(map_store)?;
    Ok(Json(json!({ "runs": runs.iter().map(|r| json!({
        "id": r.id,
        "status": r.status,
        "source": r.source,
        "cancel_requested_at": r.cancel_requested_at,
        "terminal_at": r.terminal_at,
        "created_at": r.created_at,
    })).collect::<Vec<_>>() })))
}

#[derive(Debug, Deserialize)]
pub struct CancelBody {
    #[allow(dead_code)]
    pub reason: Option<String>,
}

pub async fn cancel_run(
    State(state): State<AppState>,
    _admin: AdminAuth,
    Path((project_id, run_id)): Path<(Uuid, Uuid)>,
    body: Option<Json<CancelBody>>,
) -> Result<Json<Value>, ApiError> {
    let _ = body;
    state
        .store
        .request_cancel(project_id, run_id)
        .await
        .map_err(map_store)?;
    Ok(Json(json!({ "run_id": run_id, "cancel_requested": true })))
}

#[derive(Debug, Deserialize)]
pub struct TaskQuery {
    pub run_id: Option<Uuid>,
    pub status: Option<String>,
}

pub async fn list_tasks(
    State(state): State<AppState>,
    _admin: AdminAuth,
    Path(project_id): Path<Uuid>,
    Query(q): Query<TaskQuery>,
) -> Result<Json<Value>, ApiError> {
    let tasks = state
        .store
        .list_tasks(project_id, q.run_id, q.status.as_deref())
        .await
        .map_err(map_store)?;
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
            "block_reason": t.block_reason,
            "failure_reason": t.failure_reason,
        })).collect::<Vec<_>>()
    })))
}

pub async fn get_task(
    State(state): State<AppState>,
    _admin: AdminAuth,
    Path((project_id, task_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, ApiError> {
    let t = state
        .store
        .get_task(project_id, task_id)
        .await
        .map_err(map_store)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "task not found"))?;
    Ok(Json(json!({
        "id": t.id,
        "run_id": t.run_id,
        "plan_task_id": t.plan_task_id,
        "title": t.title,
        "status": t.status,
        "spec_md": t.spec_md,
        "depends_on": t.depends_on,
        "priority": t.priority,
        "attempt": t.attempt,
        "claimed_session_id": t.claimed_session_id,
        "progress_pct": t.progress_pct,
        "block_reason": t.block_reason,
        "hitl_notes": t.hitl_notes,
        "hitl_resume_requested_at": t.hitl_resume_requested_at,
        "failure_reason": t.failure_reason,
    })))
}

#[derive(Debug, Deserialize)]
pub struct ResumeBody {
    pub message: String,
}

pub async fn resume_task(
    State(state): State<AppState>,
    _admin: AdminAuth,
    Path((project_id, task_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<ResumeBody>,
) -> Result<Json<Value>, ApiError> {
    let ok = state
        .store
        .request_hitl_resume(project_id, task_id, &body.message)
        .await
        .map_err(map_store)?;
    if !ok {
        return Err(err(StatusCode::CONFLICT, "task not blocked"));
    }
    Ok(Json(json!({
        "task_id": task_id,
        "resume_requested": true,
    })))
}

pub async fn list_workers(
    State(state): State<AppState>,
    _admin: AdminAuth,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let sessions = state
        .store
        .list_sessions_for_project(project_id)
        .await
        .map_err(map_store)?;
    Ok(Json(json!({
        "workers": sessions.iter().map(|s| json!({
            "id": s.id,
            "board_task_id": s.board_task_id,
            "run_id": s.run_id,
            "attempt": s.attempt,
            "status": s.status,
            "runtime_backend": s.runtime_backend,
            "progress_pct": s.progress_pct,
            "last_heartbeat_at": s.last_heartbeat_at,
            "exit_reason": s.exit_reason,
        })).collect::<Vec<_>>()
    })))
}

#[derive(Debug, Deserialize)]
pub struct HeartbeatBody {
    pub seq: i32,
    pub progress_pct: Option<i32>,
}

pub async fn post_heartbeat(
    State(state): State<AppState>,
    worker: WorkerAuth,
    Path((project_id, session_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<HeartbeatBody>,
) -> Result<StatusCode, ApiError> {
    let session = state
        .store
        .authenticate_worker_session(session_id, &worker.token)
        .await
        .map_err(map_store)?
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "invalid worker session/token"))?;
    if session.project_id != project_id {
        return Err(err(StatusCode::NOT_FOUND, "session not in project"));
    }
    state
        .store
        .append_worker_report(
            project_id,
            session_id,
            session.board_task_id,
            body.seq,
            "heartbeat",
            json!({ "progress_pct": body.progress_pct }),
        )
        .await
        .map_err(map_store)?;
    Ok(StatusCode::ACCEPTED)
}

#[derive(Debug, Deserialize)]
pub struct StatusBody {
    pub seq: i32,
    pub status: String,
    pub reason: Option<String>,
}

pub async fn post_status(
    State(state): State<AppState>,
    worker: WorkerAuth,
    Path((project_id, session_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<StatusBody>,
) -> Result<StatusCode, ApiError> {
    let session = state
        .store
        .authenticate_worker_session(session_id, &worker.token)
        .await
        .map_err(map_store)?
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "invalid worker session/token"))?;
    if session.project_id != project_id {
        return Err(err(StatusCode::NOT_FOUND, "session not in project"));
    }
    state
        .store
        .append_worker_report(
            project_id,
            session_id,
            session.board_task_id,
            body.seq,
            "status",
            json!({ "status": body.status, "reason": body.reason }),
        )
        .await
        .map_err(map_store)?;
    Ok(StatusCode::ACCEPTED)
}

pub async fn admin_tick(
    State(state): State<AppState>,
    _admin: AdminAuth,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    state
        .store
        .get_project(project_id)
        .await
        .map_err(map_store)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "project not found"))?;

    let report = state.tick.tick_project(project_id).await.map_err(|e| {
        let msg = e.to_string();
        if msg.contains("lock") {
            err(StatusCode::CONFLICT, msg)
        } else {
            tracing::error!(error = %e, "tick failed");
            err(StatusCode::INTERNAL_SERVER_ERROR, msg)
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
        "external_synced": report.external_synced,
    })))
}

