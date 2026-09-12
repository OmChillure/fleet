//! Durable planner: a goal becomes an accepted `WorkPlan`. Tick stays LLM-free.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use fleet_auth::{require_user, AppError, AppResult, AuthState};
use fleet_db::repos::plan_jobs::{self, NewPlanJob};
use fleet_db::repos::workspaces as workspace_repo;
use fleet_orchestrator::domain::{
    PlanPolicy, PlanTask, WorkPlanBody, WorkPlanStatus,
};
use fleet_orchestrator::validate;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/workspaces/{workspace_id}/plans",
            get(http_list).post(http_create),
        )
        .route(
            "/api/workspaces/{workspace_id}/plans/{job_id}",
            get(http_get),
        )
}

fn auth_state(state: &AppState) -> AuthState {
    AuthState {
        db: state.db.clone(),
        config: state.auth.clone(),
    }
}

async fn require_member(
    state: &AppState,
    jar: &CookieJar,
    workspace_id: Uuid,
) -> AppResult<(fleet_db::User, String)> {
    let user = require_user(&auth_state(state), jar).await?;
    let role = workspace_repo::user_role(&state.db, workspace_id, user.id)
        .await?
        .ok_or_else(|| AppError::forbidden("not a member of this workspace"))?;
    Ok((user, role))
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreatePlanJob {
    pub goal: String,
    #[serde(default)]
    pub context: Option<Value>,
    #[serde(default)]
    pub worker_profile: Option<String>,
}

/// Pure planner: one implementer task from a goal. No LLM in the tick.
pub fn plan_from_goal(workspace_id: Uuid, goal: &str, worker_profile: &str) -> WorkPlanBody {
    let goal = goal.trim();
    let profile = worker_profile.trim();
    let profile = if profile.is_empty() {
        "generalist"
    } else {
        profile
    };
    let run_id = Uuid::now_v7();
    WorkPlanBody {
        plan_id: Uuid::now_v7(),
        project_id: workspace_id,
        run_id,
        version: 1,
        status: WorkPlanStatus::Accepted,
        source: "planner".into(),
        tasks: vec![PlanTask {
            plan_task_id: "t1".into(),
            title: goal.chars().take(120).collect(),
            summary: goal.chars().take(280).collect(),
            spec_md: format!("# Goal\n\n{goal}\n"),
            depends_on: vec![],
            worker_profile: profile.into(),
            acceptance: vec!["exists: result.md".into()],
            priority: 100,
            external_ref: None,
            timeout_s: Some(3600),
            max_retries: Some(1),
        }],
        policy: PlanPolicy::default(),
    }
}

pub async fn accept_goal_plan(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Option<Uuid>,
    goal: &str,
    worker_profile: &str,
    context: Value,
) -> AppResult<(plan_jobs::PlanJob, Uuid)> {
    let goal = goal.trim();
    if goal.is_empty() {
        return Err(AppError::bad_request("goal is required"));
    }
    let job = plan_jobs::insert(
        &state.db,
        &NewPlanJob {
            id: Uuid::now_v7(),
            workspace_id,
            created_by: user_id,
            goal: goal.to_string(),
            context,
        },
    )
    .await?;
    let claimed = plan_jobs::mark_planning(&state.db, workspace_id, job.id)
        .await?
        .ok_or_else(|| AppError::conflict("plan job is not pending"))?;
    state
        .orch
        .store
        .ensure_project(workspace_id, "workspace fleet", 3)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let plan = plan_from_goal(workspace_id, &claimed.goal, worker_profile);
    if let Err(e) = validate::validate_plan(&plan) {
        let _ = plan_jobs::mark_failed(&state.db, workspace_id, job.id, &e.to_string()).await;
        return Err(AppError::bad_request(e.to_string()));
    }
    let run_id = plan.run_id;
    match state.orch.store.insert_accepted_plan(workspace_id, plan).await {
        Ok(_) => {
            let done = plan_jobs::mark_accepted(&state.db, workspace_id, job.id, run_id)
                .await?
                .ok_or_else(|| AppError::internal("plan job vanished"))?;
            Ok((done, run_id))
        }
        Err(e) => {
            let _ = plan_jobs::mark_failed(&state.db, workspace_id, job.id, &e.to_string()).await;
            Err(AppError::bad_request(e.to_string()))
        }
    }
}

async fn http_create(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<CreatePlanJob>,
) -> AppResult<Json<Value>> {
    let (user, _) = require_member(&state, &jar, workspace_id).await?;
    let profile = body.worker_profile.unwrap_or_default();
    let ctx = body.context.unwrap_or_else(|| json!({}));
    let (job, run_id) = accept_goal_plan(
        &state,
        workspace_id,
        Some(user.id),
        &body.goal,
        &profile,
        ctx,
    )
    .await?;
    Ok(Json(json!({
        "id": job.id,
        "status": job.status,
        "plan_id": job.plan_id,
        "run_id": run_id,
        "goal": job.goal,
    })))
}

async fn http_list(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let rows = plan_jobs::list_for_workspace(&state.db, workspace_id, 50).await?;
    Ok(Json(json!({ "jobs": rows })))
}

async fn http_get(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((workspace_id, job_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let job = plan_jobs::get(&state.db, workspace_id, job_id)
        .await?
        .ok_or_else(|| AppError::not_found("plan job"))?;
    Ok(Json(json!(job)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_from_goal_is_one_valid_task() {
        let ws = Uuid::now_v7();
        let plan = plan_from_goal(ws, "Ship the weather app", "coding");
        assert_eq!(plan.project_id, ws);
        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(plan.tasks[0].worker_profile, "coding");
        assert!(plan.tasks[0].spec_md.contains("weather"));
        validate::validate_plan(&plan).expect("heuristic plan must validate");
    }

    #[test]
    fn plan_from_goal_rejects_blank_only_at_accept() {
        let plan = plan_from_goal(Uuid::now_v7(), "   x   ", "");
        assert_eq!(plan.tasks[0].worker_profile, "generalist");
        assert!(plan.tasks[0].title.contains('x'));
    }
}
