use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::auth::{AdminAuth, WorkerAuth};
use super::state::AppState;
use crate::domain::WorkPlanBody;
use crate::orch::TickError;
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

fn map_tick(e: TickError) -> ApiError {
    match e {
        TickError::Store(se) => map_store(se),
        TickError::Other(m) => {
            tracing::error!(error = %m, "tick failed");
            err(StatusCode::INTERNAL_SERVER_ERROR, m)
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
    let remote =
        crate::domain::parse_project_git_remote(body.repo_url.as_deref(), body.git_ref.as_deref())
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
    let existing = state
        .store
        .get_project_config(project_id)
        .await
        .map_err(map_store)?;
    // Same semantics as cookie PATCH …/fleet/config (Docs §6.9 / §8.2).
    let existing_remote = existing.git_remote();
    let remote = crate::domain::apply_git_remote_patch(
        existing_remote.as_ref(),
        existing.git_ref.as_str(),
        body.repo_url.as_deref(),
        body.git_ref.as_deref(),
    )
    .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
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
    let runs = state.store.list_runs(project_id).await.map_err(map_store)?;
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
    pub reason: Option<String>,
}

pub async fn cancel_run(
    State(state): State<AppState>,
    _admin: AdminAuth,
    Path((project_id, run_id)): Path<(Uuid, Uuid)>,
    body: Option<Json<CancelBody>>,
) -> Result<Json<Value>, ApiError> {
    let reason = body
        .and_then(|Json(b)| b.reason)
        .filter(|s| !s.trim().is_empty());
    state
        .store
        .request_cancel(project_id, run_id)
        .await
        .map_err(map_store)?;
    Ok(Json(json!({
        "run_id": run_id,
        "cancel_requested": true,
        "reason": reason,
    })))
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
pub struct LiveEventIn {
    #[serde(default)]
    pub ts: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct HeartbeatBody {
    pub seq: i32,
    pub progress_pct: Option<i32>,
    #[serde(default)]
    pub events: Vec<LiveEventIn>,
}

const LIVE_BATCH_MAX: usize = 64;
const LIVE_TEXT_MAX: usize = 4096;
const LIVE_KIND_MAX: usize = 64;

fn sanitize_live_events(
    raw: Vec<LiveEventIn>,
) -> Vec<(chrono::DateTime<chrono::Utc>, String, String)> {
    let mut out = Vec::new();
    for ev in raw.into_iter().take(LIVE_BATCH_MAX) {
        let text: String = ev.text.chars().take(LIVE_TEXT_MAX).collect();
        if text.trim().is_empty() {
            continue;
        }
        let kind = ev
            .kind
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("update");
        let kind: String = kind.chars().take(LIVE_KIND_MAX).collect();
        let ts = ev
            .ts
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&chrono::Utc))
            .unwrap_or_else(chrono::Utc::now);
        out.push((ts, kind, text));
    }
    out
}

async fn enqueue_live_bus(
    pool: &sqlx::PgPool,
    session_id: Uuid,
    report_seq: i32,
    events: &[(chrono::DateTime<chrono::Utc>, String, String)],
) {
    use agent_fleet_contracts::{
        AgentEvent, BusMessage, MessageBody, Producer, Redaction, SessionId, TurnId,
    };
    if events.is_empty() {
        return;
    }
    let sid = SessionId::from_uuid(session_id);
    let msg = BusMessage::new(
        Producer::Driver,
        MessageBody::AgentEvent(AgentEvent {
            session_id: sid,
            turn_id: TurnId::from_uuid(session_id),
            seq: report_seq.max(0) as u64,
            kind: "batch".into(),
            acp_method: None,
            payload: Some(json!({
                "events": events.iter().map(|(ts, kind, text)| json!({
                    "ts": ts,
                    "kind": kind,
                    "text": text,
                })).collect::<Vec<_>>()
            })),
            redaction: Redaction::None,
        }),
    );
    let message_id = msg.envelope.message_id.as_uuid();
    let Ok(body) = serde_json::to_value(&msg) else {
        return;
    };
    if let Err(e) =
        fleet_db::repos::control_plane_events::insert_event(pool, message_id, "agent.event", body)
            .await
    {
        tracing::warn!(error = %e, %session_id, "live event outbox insert failed");
    }
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
    let events = sanitize_live_events(body.events);
    let payload = json!({
        "progress_pct": body.progress_pct,
        "event_count": events.len(),
    });
    // Events first: (session_id, report_seq, idx) is idempotent. If the report
    // row is written first and events fail, a seq retry would skip them forever.
    if !events.is_empty() {
        state
            .store
            .insert_live_events(session_id, body.seq, &events)
            .await
            .map_err(map_store)?;
    }
    let inserted = state
        .store
        .append_worker_report(
            project_id,
            session_id,
            session.board_task_id,
            body.seq,
            "heartbeat",
            payload,
        )
        .await
        .map_err(map_store)?;
    if inserted && !events.is_empty() {
        enqueue_live_bus(state.store.pool(), session_id, body.seq, &events).await;
    }
    Ok(StatusCode::ACCEPTED)
}

#[derive(Debug, Deserialize)]
pub struct StatusBody {
    pub seq: i32,
    pub status: String,
    pub reason: Option<String>,
    /// Guest-written `result.md`. Inbox materializes this onto the plane
    /// session dir before `validate_done`. HTTP status alone is not proof.
    #[serde(default)]
    pub result_md: Option<String>,
    /// Optional extra relative files (`work/foo.txt` → body).
    #[serde(default)]
    pub files: Option<serde_json::Map<String, serde_json::Value>>,
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
    let result_md = body
        .result_md
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| truncate_proof(s, MAX_RESULT_MD_CHARS));
    let files = body.files.as_ref().and_then(sanitize_proof_files);
    state
        .store
        .append_worker_report(
            project_id,
            session_id,
            session.board_task_id,
            body.seq,
            "status",
            json!({
                "status": body.status,
                "reason": body.reason,
                "result_md": result_md,
                "files": files,
            }),
        )
        .await
        .map_err(map_store)?;
    Ok(StatusCode::ACCEPTED)
}

const MAX_RESULT_MD_CHARS: usize = 256 * 1024;
const MAX_PROOF_FILES: usize = 32;
const MAX_PROOF_FILE_CHARS: usize = 64 * 1024;

fn truncate_proof(s: &str, max_chars: usize) -> String {
    let mut it = s.chars();
    let mut out: String = it.by_ref().take(max_chars).collect();
    if it.next().is_some() {
        out.push('…');
    }
    out
}

fn sanitize_proof_files(
    files: &serde_json::Map<String, serde_json::Value>,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let mut out = serde_json::Map::new();
    for (k, v) in files.iter().take(MAX_PROOF_FILES) {
        let rel = k.trim().trim_start_matches(['/', '\\']);
        if rel.is_empty() || rel.split(['/', '\\']).any(|p| p == ".." || p.is_empty()) {
            continue;
        }
        let Some(text) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        out.insert(
            rel.to_string(),
            serde_json::Value::String(truncate_proof(text, MAX_PROOF_FILE_CHARS)),
        );
    }
    if out.is_empty() { None } else { Some(out) }
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

    let report = state
        .tick
        .tick_project(project_id)
        .await
        .map_err(map_tick)?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AuthMode;
    use crate::orch::TickRunner;
    use crate::store::Store;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use sqlx::postgres::PgPoolOptions;
    use std::sync::Arc;
    use std::time::Duration;
    use tower::ServiceExt;

    async fn app_with_unacquirable_pool() -> axum::Router {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_millis(50))
            .connect_lazy("postgres://unused:unused@127.0.0.1:9/none")
            .expect("lazy pool that never talks to Postgres");
        // Closed pool cannot acquire — DB-down without a live server or DATABASE_URL.
        pool.close().await;
        let store = Store::new(pool);
        let tick = Arc::new(TickRunner::new(
            store.clone(),
            std::env::temp_dir().join("fleet-orch-readyz-unacquirable"),
            "http://127.0.0.1:0",
        ));
        super::super::router(AppState {
            store,
            tick,
            admin_token: "test-admin".into(),
            auth_mode: AuthMode::Token,
        })
    }

    async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
        let res = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let v: Value = if bytes.is_empty() {
            json!({})
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| json!({"raw": String::from_utf8_lossy(&bytes)}))
        };
        (status, v)
    }

    /// Docs §8.2/§14: /healthz is liveness without DB; /readyz acquires or 503.
    #[tokio::test]
    async fn healthz_ok_without_db_readyz_503_when_pool_cannot_acquire() {
        let app = app_with_unacquirable_pool().await;

        let (st, v) = get_json(app.clone(), "/healthz").await;
        assert_eq!(st, StatusCode::OK, "healthz must stay ok with no DB: {v}");
        assert_eq!(v["status"], "ok");
        assert!(
            v.get("db").is_none(),
            "healthz must not acquire or report DB: {v}"
        );

        let (st, v) = get_json(app, "/readyz").await;
        assert_ne!(
            st,
            StatusCode::OK,
            "must not 200 ready when the pool cannot acquire: {v}"
        );
        assert_eq!(
            st,
            StatusCode::SERVICE_UNAVAILABLE,
            "readyz must be 503 when acquire fails, got {st}: {v}"
        );
        assert_ne!(
            v["status"], "ready",
            "must not 200 ready when the pool cannot acquire: {v}"
        );
        assert_eq!(v["error"], "db unavailable");
    }
}
