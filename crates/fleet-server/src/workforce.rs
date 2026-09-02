//! Workforce OS: durable goals + replace-worker.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use fleet_auth::{AppError, AppResult, AuthState, require_user};
use fleet_db::repos::workforce::{self as wf, NewGoal};
use fleet_db::repos::workspaces as workspace_repo;
use fleet_db::WorkforceGoal;
use fleet_db::PgPool;
use fleet_integrations::CredentialsKey;
use fleet_integrations::proxy::{self, ProxyCaller, ProxyRequest, ProxyResult};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/workspaces/{workspace_id}/workforce",
            get(get_workforce),
        )
        .route(
            "/api/workspaces/{workspace_id}/workforce/goals",
            post(post_goal),
        )
        .route(
            "/api/workspaces/{workspace_id}/workforce/goals/{goal_id}/cancel",
            post(post_cancel_goal),
        )
        .route(
            "/api/workspaces/{workspace_id}/fleet/sessions/{session_id}/replace",
            post(post_replace_session),
        )
}

async fn require_member(
    state: &AppState,
    jar: &CookieJar,
    workspace_id: Uuid,
) -> AppResult<(fleet_db::User, String)> {
    let auth = AuthState {
        db: state.db.clone(),
        config: state.auth.clone(),
    };
    let user = require_user(&auth, jar).await?;
    let role = workspace_repo::user_role(&state.db, workspace_id, user.id)
        .await?
        .ok_or_else(|| AppError::forbidden("not a member of this workspace"))?;
    Ok((user, role))
}

/// Run an integration op immediately. No human hold.
pub async fn gated_invoke(
    state: &AppState,
    workspace_id: Uuid,
    caller: ProxyCaller,
    provider: &str,
    operation: &str,
    args: Value,
) -> ProxyResult {
    gated_invoke_pool(
        &state.db,
        &state.integrations.credentials_key,
        workspace_id,
        caller,
        provider,
        operation,
        args,
    )
    .await
}

pub async fn gated_invoke_pool(
    db: &PgPool,
    key: &CredentialsKey,
    workspace_id: Uuid,
    caller: ProxyCaller,
    provider: &str,
    operation: &str,
    args: Value,
) -> ProxyResult {
    proxy::invoke(
        db,
        key,
        ProxyRequest {
            workspace_id,
            provider: provider.to_string(),
            operation: operation.to_string(),
            args,
            caller,
        },
    )
    .await
}

/// Attach a launched run to the matching goal, or start a new goal from the label.
pub async fn attach_run_to_goal(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Option<Uuid>,
    title: &str,
    goal_md: &str,
    run_id: Uuid,
    case_id: Option<Uuid>,
) -> AppResult<WorkforceGoal> {
    if let Some(existing) = wf::pick_goal_for_new_run(&state.db, workspace_id, run_id, case_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
    {
        if let Some(g) = wf::attach_run(&state.db, workspace_id, existing.id, run_id)
            .await
            .map_err(|e| AppError::internal(e.to_string()))?
        {
            return Ok(g);
        }
    }
    let goal = wf::insert_goal(
        &state.db,
        &NewGoal {
            id: Uuid::now_v7(),
            workspace_id,
            title: title.chars().take(200).collect(),
            goal_md: goal_md.to_string(),
            run_id: Some(run_id),
            case_id,
            created_by: user_id,
        },
    )
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;
    wf::set_goal_status(&state.db, workspace_id, goal.id, "acting", None)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
        .ok_or_else(|| AppError::not_found("goal vanished"))
}

/// Attach a case to the open goal, or start a new goal from the case title.
pub async fn attach_case_to_goal(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Option<Uuid>,
    title: &str,
    goal_md: &str,
    case_id: Uuid,
) -> AppResult<WorkforceGoal> {
    if let Some(existing) = wf::goal_by_case(&state.db, workspace_id, case_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
    {
        return Ok(existing);
    }
    if let Some(existing) = wf::latest_open_goal_without_run(&state.db, workspace_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
    {
        if existing.case_id.is_none() || existing.case_id == Some(case_id) {
            return wf::attach_case(&state.db, workspace_id, existing.id, case_id)
                .await
                .map_err(|e| AppError::internal(e.to_string()))?
                .ok_or_else(|| AppError::not_found("goal vanished"));
        }
    }
    let goal = wf::insert_goal(
        &state.db,
        &NewGoal {
            id: Uuid::now_v7(),
            workspace_id,
            title: title.chars().take(200).collect(),
            goal_md: goal_md.to_string(),
            run_id: None,
            case_id: Some(case_id),
            created_by: user_id,
        },
    )
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;
    wf::set_goal_status(&state.db, workspace_id, goal.id, "acting", None)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
        .ok_or_else(|| AppError::not_found("goal vanished"))
}

fn goal_json(g: &WorkforceGoal) -> Value {
    json!({
        "id": g.id,
        "title": g.title,
        "goal_md": g.goal_md,
        "status": g.status,
        "run_id": g.run_id,
        "case_id": g.case_id,
        "last_error": g.last_error,
        "created_at": g.created_at,
        "updated_at": g.updated_at,
    })
}

async fn get_workforce(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let goals = wf::reconcile_workspace(&state.db, workspace_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let all_goals = wf::list_goals(&state.db, workspace_id, None, 40)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(Json(json!({
        "open_goals": goals
            .iter()
            .filter(|g| matches!(g.status.as_str(), "open" | "acting" | "waiting_approval"))
            .map(goal_json)
            .collect::<Vec<_>>(),
        "goals": all_goals.iter().map(goal_json).collect::<Vec<_>>(),
    })))
}

#[derive(Debug, Deserialize)]
pub struct CreateGoalBody {
    pub title: String,
    #[serde(default)]
    pub goal_md: Option<String>,
}

async fn post_goal(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<CreateGoalBody>,
) -> AppResult<Json<Value>> {
    let (user, _) = require_member(&state, &jar, workspace_id).await?;
    let title = body.title.trim();
    if title.is_empty() {
        return Err(AppError::bad_request("title is required"));
    }
    let goal_md = body
        .goal_md
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(title)
        .to_string();

    let case = crate::cases::create_case(
        &state,
        workspace_id,
        Some(user.id),
        crate::cases::CreateCaseBody {
            title: title.to_string(),
            kind: Some("workforce".into()),
            policy_md: Some(goal_md.clone()),
            stage: Some("start".into()),
            context: Some(json!({ "source": "workforce" })),
            wake_in_s: Some(15),
        },
    )
    .await?;

    let goal = wf::insert_goal(
        &state.db,
        &NewGoal {
            id: Uuid::now_v7(),
            workspace_id,
            title: title.chars().take(200).collect(),
            goal_md,
            run_id: None,
            case_id: Some(case.id),
            created_by: Some(user.id),
        },
    )
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;
    let goal = wf::set_goal_status(&state.db, workspace_id, goal.id, "acting", None)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
        .unwrap_or(goal);

    Ok(Json(json!({
        "goal": goal_json(&goal),
        "case_id": case.id,
    })))
}

async fn post_cancel_goal(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((workspace_id, goal_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let existing = wf::get_goal(&state.db, workspace_id, goal_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
        .ok_or_else(|| AppError::not_found("goal not found"))?;
    if let Some(case_id) = existing.case_id {
        let _ = crate::cases::cancel_case(&state, workspace_id, case_id).await;
    }
    if let Some(run_id) = existing.run_id {
        let _ = state.orch.store.request_cancel(workspace_id, run_id).await;
    }
    let goal = wf::set_goal_status(&state.db, workspace_id, goal_id, "cancelled", None)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
        .ok_or_else(|| AppError::not_found("goal not found"))?;
    Ok(Json(json!({ "goal": goal_json(&goal) })))
}

async fn post_replace_session(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((workspace_id, session_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let session = state
        .orch
        .store
        .get_session(session_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
        .ok_or_else(|| AppError::not_found("session not found"))?;
    if session.project_id != workspace_id {
        return Err(AppError::not_found("session not found"));
    }
    let report = state
        .orch
        .tick
        .replace_session(workspace_id, session_id)
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    Ok(Json(json!({
        "session_id": report.session_id,
        "task_id": report.task_id,
        "replaced": report.replaced,
        "already_dead": report.already_dead,
    })))
}
