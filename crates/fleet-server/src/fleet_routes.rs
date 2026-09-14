//! Workspace-scoped fleet/orchestrator routes (cookie auth).
//!
//! Mapping: `workspace_id` == orchestrator `project_id`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use fleet_auth::{AppError, AppResult, AuthState, require_user};
use fleet_db::repos::workspaces as workspace_repo;
use fleet_orchestrator::domain::{
    OnTaskFailPolicy, PlanPolicy, PlanTask, WorkPlanBody, WorkPlanStatus,
};
use fleet_orchestrator::orch::TickError;
use fleet_orchestrator::store::StoreError;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/workspaces/{workspace_id}/fleet",
            get(get_fleet_project),
        )
        .route(
            "/api/workspaces/{workspace_id}/fleet/config",
            patch(patch_fleet_config),
        )
        .route(
            "/api/workspaces/{workspace_id}/fleet/settings",
            get(get_settings).patch(patch_settings),
        )
        .route(
            "/api/workspaces/{workspace_id}/fleet/personas",
            get(get_personas),
        )
        .route(
            "/api/workspaces/{workspace_id}/fleet/runs/fixture",
            post(post_fixture),
        )
        .route("/api/workspaces/{workspace_id}/fleet/tick", post(post_tick))
        .route("/api/workspaces/{workspace_id}/fleet/tasks", get(get_tasks))
        .route("/api/workspaces/{workspace_id}/fleet/runs", get(get_runs))
        .route(
            "/api/workspaces/{workspace_id}/fleet/sessions",
            get(get_sessions),
        )
        .route(
            "/api/workspaces/{workspace_id}/fleet/sessions/{session_id}",
            get(get_session_detail),
        )
        .route(
            "/api/workspaces/{workspace_id}/fleet/workers",
            get(get_workers),
        )
        .route(
            "/api/workspaces/{workspace_id}/fleet/overview",
            get(get_overview),
        )
        .route(
            "/api/workspaces/{workspace_id}/fleet/runs/{run_id}/cancel",
            post(post_cancel),
        )
        .route(
            "/api/workspaces/{workspace_id}/fleet/tasks/{task_id}/resume",
            post(post_resume),
        )
        .route(
            "/api/workspaces/{workspace_id}/fleet/sessions/{session_id}/kill",
            post(post_kill_session),
        )
}

pub(crate) async fn require_member(
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
        .ensure_project(workspace_id, name_hint, 16)
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
    let name = format!(
        "{} fleet",
        user.email.split('@').next().unwrap_or("workspace")
    );
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
        "config": settings_json(&state, workspace_id, &cfg).await,
    })))
}

async fn settings_json(
    state: &AppState,
    workspace_id: Uuid,
    cfg: &fleet_orchestrator::store::ProjectConfigRow,
) -> Value {
    let llm = fleet_orchestrator::sandbox::agent_loop::resolve_workspace_llm(
        &cfg.fleet_llm,
        &cfg.fleet_llm_model,
    );
    let grok_env = fleet_orchestrator::sandbox::agent_loop::grok_env_enabled();
    let min_isolation = fleet_db::repos::workspaces::min_isolation(&state.db, workspace_id)
        .await
        .unwrap_or_else(|_| "process".into());
    // Do not serialize OpenaiConfig / ClaudeConfig / GrokConfig — api_key stays off the wire.
    json!({
        "max_agents": cfg.max_agents,
        "max_agents_cap": fleet_db::repos::instance_settings::get(&state.db)
            .await
            .map(|s| s.max_agents_cap)
            .unwrap_or(128),
        "min_isolation": min_isolation,
        "workspace_location": min_isolation,
        "agent_runtime": workspace_repo::default_run_mode(&state.db, workspace_id)
            .await
            .unwrap_or_else(|_| "on_pc".into()),
        "default_run_mode": workspace_repo::default_run_mode(&state.db, workspace_id)
            .await
            .unwrap_or_else(|_| "on_pc".into()),
        "default_send_policy": workspace_repo::default_send_policy(&state.db, workspace_id)
            .await
            .unwrap_or_else(|_| "owner_only".into()),
        "chat_max_turns": workspace_repo::chat_max_turns(&state.db, workspace_id)
            .await
            .unwrap_or(workspace_repo::CHAT_MAX_TURNS_DEFAULT),
        "chat_max_turns_min": workspace_repo::CHAT_MAX_TURNS_MIN,
        "chat_max_turns_max": workspace_repo::CHAT_MAX_TURNS_MAX,
        "repo_url": cfg.repo_url,
        "git_ref": cfg.git_ref,
        "fleet_llm": cfg.fleet_llm,
        "fleet_llm_model": cfg.fleet_llm_model,
        "grok_env": grok_env,
        "effective_backend": llm.backend.as_deref().unwrap_or("off"),
        "effective_model": llm.model,
    })
}

async fn get_settings(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    ensure_project(&state, workspace_id, "workspace fleet").await?;
    let cfg = state
        .orch
        .store
        .get_project_config(workspace_id)
        .await
        .map_err(|e| map_orch_err("get_project_config", e))?;
    Ok(Json(settings_json(&state, workspace_id, &cfg).await))
}

#[derive(Debug, Deserialize)]
pub struct PatchSettingsBody {
    pub max_agents: Option<i32>,
    pub fleet_llm: Option<String>,
    pub fleet_llm_model: Option<String>,
    pub min_isolation: Option<String>,
    pub default_run_mode: Option<String>,
    pub default_send_policy: Option<String>,
    pub chat_max_turns: Option<i32>,
}

async fn patch_settings(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<PatchSettingsBody>,
) -> AppResult<Json<Value>> {
    let (_user, role) = require_member(&state, &jar, workspace_id).await?;
    ensure_project(&state, workspace_id, "workspace fleet").await?;
    let current = state
        .orch
        .store
        .get_project_config(workspace_id)
        .await
        .map_err(|e| map_orch_err("get_project_config", e))?;

    let max = body.max_agents.unwrap_or(current.max_agents);
    let cap = fleet_db::repos::instance_settings::get(&state.db)
        .await
        .map(|s| s.max_agents_cap)
        .unwrap_or(128)
        .clamp(1, fleet_orchestrator::domain::MAX_PROJECT_AGENTS);
    if max < 1 || max > cap {
        return Err(AppError::bad_request(format!(
            "max_agents must be 1–{cap} (instance cap)"
        )));
    }
    let llm = body
        .fleet_llm
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(current.fleet_llm.as_str());
    if !matches!(llm, "off" | "ollama") {
        return Err(AppError::bad_request(
            "fleet_llm must be off or ollama (Grok uses composer / llm-keys / FLEET_AGENT_BACKEND)",
        ));
    }
    let model = body
        .fleet_llm_model
        .clone()
        .unwrap_or_else(|| current.fleet_llm_model.clone());

    if body.min_isolation.is_some() && role != "owner" && role != "admin" {
        return Err(AppError::forbidden(
            "owner or admin required to change min_isolation",
        ));
    }
    if let Some(raw) = body.min_isolation.as_deref() {
        workspace_repo::set_min_isolation(&state.db, workspace_id, raw).await?;
    }
    if body.default_run_mode.is_some() || body.default_send_policy.is_some() {
        if role != "owner" && role != "admin" {
            return Err(AppError::forbidden(
                "owner or admin required to change default run mode",
            ));
        }
    }
    if let Some(raw) = body.default_run_mode.as_deref() {
        workspace_repo::set_default_run_mode(&state.db, workspace_id, raw).await?;
    }
    if let Some(raw) = body.default_send_policy.as_deref() {
        workspace_repo::set_default_send_policy(&state.db, workspace_id, raw).await?;
    }
    if let Some(turns) = body.chat_max_turns {
        if workspace_repo::parse_chat_max_turns(turns).is_none() {
            return Err(AppError::bad_request(format!(
                "chat_max_turns must be {}–{}",
                workspace_repo::CHAT_MAX_TURNS_MIN,
                workspace_repo::CHAT_MAX_TURNS_MAX
            )));
        }
        workspace_repo::set_chat_max_turns(&state.db, workspace_id, turns).await?;
    }

    let cfg = state
        .orch
        .store
        .update_fleet_settings(workspace_id, max, llm, &model)
        .await
        .map_err(|e| map_orch_err("update_fleet_settings", e))?;
    Ok(Json(settings_json(&state, workspace_id, &cfg).await))
}

async fn get_personas(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let data_dir = std::env::var("DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("./data"));
    let personas = fleet_orchestrator::personas::list_personas(&data_dir).await;
    let (effective, grok_env) = match state.orch.store.get_project_config(workspace_id).await {
        Ok(cfg) => {
            let llm = fleet_orchestrator::sandbox::agent_loop::resolve_workspace_llm(
                &cfg.fleet_llm,
                &cfg.fleet_llm_model,
            );
            (
                llm.backend,
                fleet_orchestrator::sandbox::agent_loop::grok_env_enabled(),
            )
        }
        Err(_) => (
            fleet_orchestrator::sandbox::agent_loop::resolve_workspace_llm("off", "").backend,
            fleet_orchestrator::sandbox::agent_loop::grok_env_enabled(),
        ),
    };
    Ok(Json(json!({
        "source": "https://github.com/msitarzewski/agency-agents",
        "agent_backend": effective,
        "grok_env": grok_env,
        "count": personas.len(),
        "personas": personas,
    })))
}

#[derive(Debug, Deserialize)]
pub struct PatchFleetConfigRequest {
    pub repo_url: Option<String>,
    pub git_ref: Option<String>,
}

async fn patch_fleet_config(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<PatchFleetConfigRequest>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    ensure_project(&state, workspace_id, "workspace fleet").await?;
    let existing = state
        .orch
        .store
        .get_project_config(workspace_id)
        .await
        .map_err(|e| map_orch_err("get_project_config", e))?;
    let existing_remote = existing.git_remote();
    let remote = fleet_orchestrator::domain::apply_git_remote_patch(
        existing_remote.as_ref(),
        existing.git_ref.as_str(),
        body.repo_url.as_deref(),
        body.git_ref.as_deref(),
    )
    .map_err(|e| AppError::bad_request(e.to_string()))?;
    let cfg = state
        .orch
        .store
        .set_project_git_remote(workspace_id, remote.as_ref())
        .await
        .map_err(|e| map_orch_err("set_project_git_remote", e))?;
    Ok(Json(json!({
        "project_id": workspace_id,
        "config": {
            "max_agents": cfg.max_agents,
            "repo_url": cfg.repo_url,
            "git_ref": cfg.git_ref,
        }
    })))
}

#[derive(Debug, Deserialize)]
pub struct FixtureRequest {
    /// `chain` (default) or `diamond`.
    #[serde(default)]
    pub kind: Option<String>,
    /// `on_pc` | `in_container`. Default: workspace `default_run_mode`.
    #[serde(default)]
    pub run_mode: Option<String>,
}

fn fixture_tasks(kind: &str) -> Option<Vec<PlanTask>> {
    let coding = |id: &str, title: &str, deps: Vec<&str>, priority: i32| PlanTask {
        plan_task_id: id.into(),
        title: title.into(),
        summary: String::new(),
        spec_md: String::new(),
        depends_on: deps.into_iter().map(str::to_string).collect(),
        worker_profile: "coding".into(),
        acceptance: vec![],
        priority,
        external_ref: None,
        timeout_s: Some(3600),
        max_retries: Some(1),
    };
    match kind {
        "diamond" => Some(vec![
            coding("root", "Root", vec![], 100),
            coding("left", "Left branch", vec!["root"], 50),
            coding("right", "Right branch", vec!["root"], 50),
            coding("join", "Join", vec!["left", "right"], 10),
        ]),
        "chain" => Some(vec![
            PlanTask {
                plan_task_id: "t1".into(),
                title: "Root task".into(),
                summary: "Prepare workspace artifacts for the child task.".into(),
                spec_md:
                    "# Root task\n\nWrite a short plan artifact, then succeed so `t2` can run.\n"
                        .into(),
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
                spec_md: "# Child task\n\nDepends on `t1`. Produce a final summary artifact.\n"
                    .into(),
                depends_on: vec!["t1".into()],
                worker_profile: "coding".into(),
                acceptance: vec!["chain complete".into()],
                priority: 50,
                external_ref: None,
                timeout_s: Some(3600),
                max_retries: None,
            },
        ]),
        _ => None,
    }
}

async fn post_fixture(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<FixtureRequest>,
) -> AppResult<(StatusCode, Json<Value>)> {
    let (user, _) = require_member(&state, &jar, workspace_id).await?;
    let name = format!(
        "{} fleet",
        user.email.split('@').next().unwrap_or("workspace")
    );
    ensure_project(&state, workspace_id, &name).await?;

    // Empty/omitted kind stays "chain". Unknown non-empty kinds 400 fail-closed.
    let kind = body
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("chain");
    let tasks = fixture_tasks(kind)
        .ok_or_else(|| AppError::bad_request(format!("unknown fixture kind: {kind}")))?;
    let run_id = Uuid::new_v4();
    let plan_id = Uuid::new_v4();
    let ws_default = workspace_repo::default_run_mode(&state.db, workspace_id)
        .await
        .unwrap_or_else(|_| fleet_db::WORKER_HOST_RUN_ON_PC.to_string());
    let run_mode = match body
        .run_mode
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(raw) => fleet_db::parse_worker_host_run_mode(raw)
            .ok_or_else(|| AppError::bad_request("run_mode must be on_pc or in_container"))?
            .to_string(),
        None => ws_default,
    };
    let plan = WorkPlanBody {
        plan_id,
        project_id: workspace_id,
        run_id,
        version: 1,
        status: WorkPlanStatus::Accepted,
        source: "ui".into(),
        tasks,
        policy: PlanPolicy {
            max_agents_override: None,
            on_task_fail: OnTaskFailPolicy::FailRun,
            auto_sync_external: false,
            llm_provider: None,
            llm_model: None,
            requested_by: Some(user.id),
            run_mode,
            ..Default::default()
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
            "kind": kind,
        })),
    ))
}

async fn post_tick(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let (user, _) = require_member(&state, &jar, workspace_id).await?;
    let name = format!(
        "{} fleet",
        user.email.split('@').next().unwrap_or("workspace")
    );
    ensure_project(&state, workspace_id, &name).await?;

    let report = state
        .orch
        .tick
        .tick_project(workspace_id)
        .await
        .map_err(|e| match e {
            TickError::Store(StoreError::Conflict(msg)) => {
                tracing::warn!(%workspace_id, error = %msg, "tick conflict (lock)");
                AppError::conflict(msg)
            }
            e => {
                let msg = e.to_string();
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

async fn get_overview(
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
    let tasks = state
        .orch
        .store
        .list_tasks_slim(workspace_id, None)
        .await
        .map_err(|e| map_orch_err("list_tasks", e))?;
    let sessions = state
        .orch
        .store
        .list_sessions_for_project(workspace_id)
        .await
        .map_err(|e| map_orch_err("list_sessions", e))?;

    let open_run = runs
        .iter()
        .find(|r| !matches!(r.status.as_str(), "completed" | "failed" | "cancelled"));
    let focus = open_run.or(runs.first());
    let run_id = focus.map(|r| r.id);

    let mut chat = Vec::new();
    if let Some(rid) = run_id {
        let talk = fleet_orchestrator::sandbox::team_chat::TeamTalk::from_spawn(
            state.orch.tick.data_dir(),
            workspace_id,
            rid,
            "human",
            "human",
        );
        chat = fleet_orchestrator::sandbox::team_chat::list_messages(&talk).await;
    }

    let replied: std::collections::HashSet<uuid::Uuid> =
        chat.iter().filter_map(|m| m.in_reply_to).collect();
    let waiting: Vec<Value> = chat
        .iter()
        .filter(|m| m.kind == "ask" && !replied.contains(&m.id))
        .map(|m| {
            json!({
                "id": m.id,
                "from": m.from,
                "from_profile": m.from_profile,
                "to": m.to,
                "body": m.body,
                "ts": m.ts,
            })
        })
        .collect();

    let live_agents = sessions
        .iter()
        .filter(|s| {
            matches!(
                s.status.as_str(),
                "spawning" | "healthy" | "stuck" | "killing"
            )
        })
        .count();
    let in_progress = tasks
        .iter()
        .filter(|t| matches!(t.status.as_str(), "assigned" | "in_progress"))
        .count();
    let changes = tasks
        .iter()
        .filter(|t| t.failure_reason.as_deref() == Some("changes_requested"))
        .count();

    let live_ids: Vec<uuid::Uuid> = sessions.iter().map(|s| s.id).collect();
    let mut activity = state
        .orch
        .store
        .list_recent_live_activity(&live_ids, 40)
        .await
        .map_err(|e| map_orch_err("list_live_activity", e))?;
    for ev in &mut activity {
        let Some(sid) = ev
            .get("session_id")
            .and_then(|v| v.as_str())
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
        else {
            continue;
        };
        if let Some(s) = sessions.iter().find(|x| x.id == sid) {
            ev.as_object_mut().map(|o| {
                o.insert("profile".into(), json!(s.worker_profile));
                o.insert("status".into(), json!(s.status));
            });
        }
    }

    Ok(Json(json!({
        "open_runs": runs.iter().filter(|r| {
            !matches!(r.status.as_str(), "completed" | "failed" | "cancelled")
        }).count(),
        "total_runs": runs.len(),
        "live_agents": live_agents,
        "tasks_in_progress": in_progress,
        "changes_requested": changes,
        "focus_run_id": run_id,
        "runs": runs.iter().take(8).map(|r| json!({
            "id": r.id,
            "status": r.status,
            "source": r.source,
            "created_at": r.created_at,
        })).collect::<Vec<_>>(),
        "tasks": tasks.iter().filter(|t| run_id.map(|id| t.run_id == id).unwrap_or(true)).take(40).map(|t| json!({
            "id": t.id,
            "run_id": t.run_id,
            "plan_task_id": t.plan_task_id,
            "title": t.title,
            "status": t.status,
            "worker_profile": t.worker_profile,
            "failure_reason": t.failure_reason,
            "attempt": t.attempt,
            "claimed_session_id": t.claimed_session_id,
        })).collect::<Vec<_>>(),
        "waiting": waiting,
        "activity": activity,
        "chat": chat.iter().rev().take(30).rev().map(|m| json!({
            "id": m.id,
            "seq": m.seq,
            "ts": m.ts,
            "from": m.from,
            "from_profile": m.from_profile,
            "to": m.to,
            "kind": m.kind,
            "in_reply_to": m.in_reply_to,
            "body": m.body,
        })).collect::<Vec<_>>(),
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
        .list_tasks_slim(workspace_id, None)
        .await
        .map_err(|e| map_orch_err("list_tasks", e))?;
    Ok(Json(json!({
        "tasks": tasks.iter().map(|t| json!({
            "id": t.id,
            "run_id": t.run_id,
            "plan_task_id": t.plan_task_id,
            "title": t.title,
            "status": t.status,
            "depends_on": t.depends_on,
            "priority": t.priority,
            "attempt": t.attempt,
            "progress_pct": t.progress_pct,
            "claimed_session_id": t.claimed_session_id,
            "worker_profile": t.worker_profile,
            "block_reason": t.block_reason,
            "hitl_notes": t.hitl_notes,
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

async fn get_session_detail(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((workspace_id, session_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    ensure_project(&state, workspace_id, "workspace fleet").await?;
    let session = state
        .orch
        .store
        .get_session(session_id)
        .await
        .map_err(|e| map_orch_err("get_session", e))?
        .ok_or_else(|| AppError::not_found("session"))?;
    if session.project_id != workspace_id {
        return Err(AppError::not_found("session"));
    }
    let work_dir = session.work_dir.clone().unwrap_or_default();
    let live = state
        .orch
        .store
        .list_live_events(session_id, 500)
        .await
        .map_err(|e| map_orch_err("list_live_events", e))?;
    let mut transcript = read_capped_file(
        std::path::Path::new(&work_dir).join("artifacts/transcript.md"),
        80_000,
    );
    if transcript.trim().is_empty() {
        transcript = read_capped_file(std::path::Path::new(&work_dir).join("stdout.log"), 80_000);
    }
    let prompt = read_capped_file(std::path::Path::new(&work_dir).join("prompt.md"), 16_000);
    let result =
        std::fs::read_to_string(std::path::Path::new(&work_dir).join("artifacts/result.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok());
    Ok(Json(json!({
        "session": {
            "id": session.id,
            "run_id": session.run_id,
            "board_task_id": session.board_task_id,
            "attempt": session.attempt,
            "status": session.status,
            "runtime_backend": session.runtime_backend,
            "worker_profile": session.worker_profile,
            "progress_pct": session.progress_pct,
            "last_heartbeat_at": session.last_heartbeat_at,
            "exit_reason": session.exit_reason,
            "work_dir": session.work_dir,
            "started_at": session.started_at,
            "exited_at": session.exited_at,
        },
        "prompt": prompt,
        "live": live,
        "transcript": transcript,
        "result": result,
    })))
}

fn read_capped_file(path: impl AsRef<std::path::Path>, max: usize) -> String {
    let Ok(s) = std::fs::read_to_string(path) else {
        return String::new();
    };
    if s.len() <= max {
        return s;
    }
    s.chars()
        .rev()
        .take(max)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
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

/// Worker sessions exposed as "sandboxes" for product language (stub backend = session).
async fn get_workers(
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
        .map_err(|e| map_orch_err("list_sessions_for_project", e))?;
    Ok(Json(json!({
        "sandboxes": sessions.iter().map(|s| json!({
            "id": s.id,
            "run_id": s.run_id,
            "board_task_id": s.board_task_id,
            "attempt": s.attempt,
            "status": s.status,
            "runtime_backend": s.runtime_backend,
            "runtime_handle": s.runtime_handle,
            "worker_profile": s.worker_profile,
            "progress_pct": s.progress_pct,
            "work_dir": s.work_dir,
            "last_heartbeat_at": s.last_heartbeat_at,
            "started_at": s.started_at,
            "exited_at": s.exited_at,
            "exit_reason": s.exit_reason,
            "created_at": s.created_at,
        })).collect::<Vec<_>>()
    })))
}

#[derive(Debug, Deserialize)]
pub struct CancelBody {
    pub reason: Option<String>,
}

async fn post_cancel(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((workspace_id, run_id)): Path<(Uuid, Uuid)>,
    body: Option<Json<CancelBody>>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let reason = body
        .and_then(|Json(b)| b.reason)
        .filter(|s| !s.trim().is_empty());
    state
        .orch
        .store
        .request_cancel(workspace_id, run_id)
        .await
        .map_err(|e| {
            tracing::error!(%workspace_id, %run_id, error = %e, "request_cancel failed");
            AppError::bad_request(e.to_string())
        })?;
    let _ = fleet_db::repos::tickets::reopen_tickets_for_run(&state.db, workspace_id, run_id).await;
    Ok(Json(json!({
        "run_id": run_id,
        "cancel_requested": true,
        "reason": reason,
    })))
}

#[derive(Debug, Deserialize)]
pub struct ResumeBody {
    #[serde(default)]
    pub message: Option<String>,
}

async fn post_resume(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((workspace_id, task_id)): Path<(Uuid, Uuid)>,
    body: Option<Json<ResumeBody>>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    ensure_project(&state, workspace_id, "workspace fleet").await?;
    let message = body
        .and_then(|Json(b)| b.message)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "resumed from UI".into());
    let ok = state
        .orch
        .store
        .request_hitl_resume(workspace_id, task_id, &message)
        .await
        .map_err(|e| map_orch_err("request_hitl_resume", e))?;
    if !ok {
        return Err(AppError::conflict("task is not blocked"));
    }
    // Tick so the resume is applied immediately (blocked → ready).
    if let Err(e) = state.orch.tick.tick_project(workspace_id).await {
        let msg = e.to_string();
        if !msg.contains("lock") {
            tracing::warn!(%workspace_id, %task_id, error = %msg, "resume tick failed");
        }
    }
    Ok(Json(json!({
        "task_id": task_id,
        "resume_requested": true,
        "message": message,
    })))
}

#[derive(Debug, Deserialize)]
pub struct KillSessionBody {
    pub reason: Option<String>,
}

async fn post_kill_session(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((workspace_id, session_id)): Path<(Uuid, Uuid)>,
    body: Option<Json<KillSessionBody>>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    ensure_project(&state, workspace_id, "workspace fleet").await?;
    let report = state
        .orch
        .tick
        .kill_session(workspace_id, session_id)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("not found") {
                AppError::not_found(msg)
            } else {
                map_orch_err("kill_session", e)
            }
        })?;
    let reason = body.and_then(|Json(b)| b.reason);
    Ok(Json(json!({
        "session_id": report.session_id,
        "killed": !report.already_dead,
        "already_dead": report.already_dead,
        "signaled": report.signaled,
        "reason": reason,
    })))
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
