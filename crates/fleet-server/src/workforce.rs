//! Workforce OS: durable goals + replace-worker.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use fleet_auth::{AppError, AppResult, AuthState, require_user};
use fleet_db::PgPool;
use fleet_db::WorkforceGoal;
use fleet_db::repos::workforce::{self as wf, NewGoal};
use fleet_db::repos::workspaces as workspace_repo;
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
            "/api/workspaces/{workspace_id}/workforce/intake",
            post(post_intake),
        )
        .route(
            "/api/workspaces/{workspace_id}/workforce/approvals/{approval_id}/decide",
            post(post_decide_approval),
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

/// Run an integration op. Merge, outbound messages, and deploys are held
/// until an owner or admin decides. Everything else runs immediately.
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
    let provider = provider.trim().to_ascii_lowercase();
    let operation = operation.trim().to_ascii_lowercase();
    if let Some(kind) = crate::workforce_os::gate_kind(&provider, &operation) {
        return hold_for_approval(db, workspace_id, caller, kind, &provider, &operation, args)
            .await;
    }
    proxy::invoke(
        db,
        key,
        ProxyRequest {
            workspace_id,
            provider,
            operation,
            args,
            caller,
        },
    )
    .await
}

async fn hold_for_approval(
    db: &PgPool,
    workspace_id: Uuid,
    caller: ProxyCaller,
    kind: &str,
    provider: &str,
    operation: &str,
    args: Value,
) -> ProxyResult {
    let args = crate::workforce_os::redact_args(&args);
    let existing = wf::find_pending_approval(db, workspace_id, provider, operation, &args)
        .await
        .ok()
        .flatten();
    if let Some(row) = existing {
        return held_result(provider, operation, kind, row.id);
    }
    let task_id = wf::block_latest_live_task(
        db,
        workspace_id,
        "needs_approval",
        &format!("{kind} is waiting for a person ({provider} {operation})"),
    )
    .await
    .ok()
    .flatten();
    let goal = wf::latest_open_goal(db, workspace_id).await.ok().flatten();
    let goal_id = goal.as_ref().map(|g| g.id);
    let summary = format!("{kind} via {provider} {operation} is waiting for a person");
    let approval = match wf::insert_approval(
        db,
        &wf::NewApproval {
            id: Uuid::now_v7(),
            workspace_id,
            goal_id,
            task_id,
            kind: kind.to_string(),
            provider: provider.to_string(),
            operation: operation.to_string(),
            args: args.clone(),
            summary: summary.clone(),
        },
    )
    .await
    {
        Ok(row) => row,
        Err(e) => {
            return ProxyResult::err(provider, operation, "policy_gate", e.to_string(), None);
        }
    };
    if let Some(gid) = goal_id {
        let _ = wf::set_goal_status(db, workspace_id, gid, "waiting_approval", None).await;
    }
    record_event(
        db,
        "workforce.policy_held",
        json!({
            "workspace_id": workspace_id,
            "approval_id": approval.id,
            "goal_id": goal_id,
            "task_id": task_id,
            "kind": kind,
            "provider": provider,
            "operation": operation,
            "caller": caller.kind(),
        }),
    )
    .await;
    held_result(provider, operation, kind, approval.id)
}

fn held_result(provider: &str, operation: &str, kind: &str, approval_id: Uuid) -> ProxyResult {
    ProxyResult {
        ok: false,
        summary: format!("Held for human approval ({kind}). Nothing was sent."),
        data: json!({
            "approval_id": approval_id,
            "kind": kind,
            "status": "pending",
        }),
        provider_status: None,
        error_code: Some("awaiting_approval".into()),
        provider: provider.to_string(),
        operation: operation.to_string(),
    }
}

async fn record_event(db: &PgPool, event_type: &str, body: Value) {
    if let Err(e) =
        fleet_db::repos::control_plane_events::insert_outbound(db, Uuid::now_v7(), event_type, body)
            .await
    {
        tracing::warn!(%event_type, error = %e, "workforce event was not recorded");
    }
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
            source_ref: None,
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
            source_ref: None,
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
        "source_ref": g.source_ref,
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
    let pending = wf::list_pending_approvals(&state.db, workspace_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(Json(json!({
        "open_goals": goals
            .iter()
            .filter(|g| matches!(g.status.as_str(), "open" | "acting" | "waiting_approval"))
            .map(goal_json)
            .collect::<Vec<_>>(),
        "goals": all_goals.iter().map(goal_json).collect::<Vec<_>>(),
        "pending_approvals": pending.iter().map(approval_json).collect::<Vec<_>>(),
    })))
}

fn approval_json(a: &fleet_db::PolicyApproval) -> Value {
    json!({
        "id": a.id,
        "goal_id": a.goal_id,
        "task_id": a.task_id,
        "kind": a.kind,
        "provider": a.provider,
        "operation": a.operation,
        "summary": a.summary,
        "status": a.status,
        "created_at": a.created_at,
    })
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
            source_ref: None,
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

#[derive(Debug, Deserialize)]
struct DecideBody {
    decision: String,
}

async fn post_decide_approval(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((workspace_id, approval_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<DecideBody>,
) -> AppResult<Json<Value>> {
    let (user, role) = require_member(&state, &jar, workspace_id).await?;
    if role != "owner" && role != "admin" {
        return Err(AppError::forbidden(
            "only an owner or admin can decide a policy gate",
        ));
    }
    let decision = body.decision.trim().to_ascii_lowercase();
    let status = match decision.as_str() {
        "approve" => "approved",
        "deny" => "denied",
        _ => return Err(AppError::bad_request("decision must be approve or deny")),
    };
    let row = wf::decide_approval(&state.db, workspace_id, approval_id, status, user.id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
        .ok_or_else(|| AppError::not_found("approval is not pending"))?;
    if status == "denied" {
        release_gate(&state, workspace_id, &row, "A person denied this gate.").await;
        record_event(
            &state.db,
            "workforce.policy_denied",
            json!({
                "workspace_id": workspace_id,
                "approval_id": row.id,
                "kind": row.kind,
                "decided_by": user.id,
            }),
        )
        .await;
        return Ok(Json(json!({ "approval": approval_json(&row) })));
    }
    let result = proxy::invoke(
        &state.db,
        &state.integrations.credentials_key,
        ProxyRequest {
            workspace_id,
            provider: row.provider.clone(),
            operation: row.operation.clone(),
            args: row.args.clone(),
            caller: ProxyCaller::Chat { user_id: user.id },
        },
    )
    .await;
    let finish = if result.ok { "executed" } else { "failed" };
    let stored = json!({
        "ok": result.ok,
        "summary": result.summary,
        "error_code": result.error_code,
    });
    let finished = wf::finish_approval(&state.db, workspace_id, row.id, finish, &stored)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
        .unwrap_or(row.clone());
    release_gate(
        &state,
        workspace_id,
        &finished,
        "A person approved this gate.",
    )
    .await;
    record_event(
        &state.db,
        "workforce.policy_decided",
        json!({
            "workspace_id": workspace_id,
            "approval_id": finished.id,
            "kind": finished.kind,
            "status": finish,
            "ok": result.ok,
            "decided_by": user.id,
        }),
    )
    .await;
    Ok(Json(json!({
        "approval": approval_json(&finished),
        "result": stored,
    })))
}

async fn release_gate(
    state: &AppState,
    workspace_id: Uuid,
    row: &fleet_db::PolicyApproval,
    note: &str,
) {
    if let Some(task_id) = row.task_id {
        let _ = wf::resume_blocked_task(&state.db, workspace_id, task_id, note).await;
    }
    if let Some(goal_id) = row.goal_id {
        let _ = wf::set_goal_status(&state.db, workspace_id, goal_id, "acting", None).await;
    }
}

async fn post_intake(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let (user, _) = require_member(&state, &jar, workspace_id).await?;
    let repos = gated_invoke(
        &state,
        workspace_id,
        ProxyCaller::Chat { user_id: user.id },
        "github",
        "list_repos",
        json!({ "per_page": 5 }),
    )
    .await;
    if !repos.ok {
        record_event(
            &state.db,
            "workforce.intake_refused",
            json!({
                "workspace_id": workspace_id,
                "reason": repos.summary,
            }),
        )
        .await;
        return Ok(Json(json!({
            "accepted": [],
            "refused": [{ "source_ref": "github", "reason": repos.summary }],
            "already_open": [],
        })));
    }
    let repo_items = repos
        .data
        .get("items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut accepted = Vec::new();
    let mut refused = Vec::new();
    let mut already_open = Vec::new();
    for repo in repo_items.iter().take(5) {
        if accepted.len() >= 20 {
            break;
        }
        let full = repo.get("full_name").and_then(|v| v.as_str()).unwrap_or("");
        let Some((owner, name)) = full.split_once('/') else {
            continue;
        };
        let issues = gated_invoke(
            &state,
            workspace_id,
            ProxyCaller::Chat { user_id: user.id },
            "github",
            "list_issues",
            json!({ "owner": owner, "repo": name, "state": "open", "per_page": 8 }),
        )
        .await;
        if !issues.ok {
            refused.push(json!({
                "source_ref": format!("github:{full}"),
                "reason": issues.summary,
            }));
            continue;
        }
        let issue_items = issues
            .data
            .get("items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        for issue in issue_items.iter().take(8) {
            if accepted.len() >= 20 {
                break;
            }
            let number = issue.get("number").and_then(|v| v.as_u64()).unwrap_or(0);
            let title = issue.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let body = issue.get("body").and_then(|v| v.as_str()).unwrap_or("");
            let url = issue.get("html_url").and_then(|v| v.as_str()).unwrap_or("");
            match crate::workforce_os::analyze_github_issue(full, number, title, body, url) {
                crate::workforce_os::IntakeVerdict::Refuse { source_ref, reason } => {
                    refused.push(json!({ "source_ref": source_ref, "reason": reason }));
                }
                crate::workforce_os::IntakeVerdict::Accept {
                    source_ref,
                    title,
                    goal_md,
                } => {
                    if wf::goal_by_source_ref(&state.db, workspace_id, &source_ref)
                        .await
                        .map_err(|e| AppError::internal(e.to_string()))?
                        .is_some()
                    {
                        already_open.push(source_ref);
                        continue;
                    }
                    let case = crate::cases::create_case(
                        &state,
                        workspace_id,
                        Some(user.id),
                        crate::cases::CreateCaseBody {
                            title: title.clone(),
                            kind: Some("workforce".into()),
                            policy_md: Some(goal_md.clone()),
                            stage: Some("start".into()),
                            context: Some(json!({
                                "source": "workforce_intake",
                                "source_ref": source_ref,
                            })),
                            wake_in_s: Some(15),
                        },
                    )
                    .await?;
                    let inserted = wf::insert_goal(
                        &state.db,
                        &NewGoal {
                            id: Uuid::now_v7(),
                            workspace_id,
                            title: title.clone(),
                            goal_md,
                            run_id: None,
                            case_id: Some(case.id),
                            created_by: Some(user.id),
                            source_ref: Some(source_ref.clone()),
                        },
                    )
                    .await;
                    let goal = match inserted {
                        Ok(goal) => goal,
                        Err(e) if e.is_unique_violation() => {
                            already_open.push(source_ref);
                            continue;
                        }
                        Err(e) => return Err(AppError::internal(e.to_string())),
                    };
                    let goal =
                        wf::set_goal_status(&state.db, workspace_id, goal.id, "acting", None)
                            .await
                            .map_err(|e| AppError::internal(e.to_string()))?
                            .unwrap_or(goal);
                    record_event(
                        &state.db,
                        "workforce.intake_accepted",
                        json!({
                            "workspace_id": workspace_id,
                            "goal_id": goal.id,
                            "source_ref": source_ref,
                        }),
                    )
                    .await;
                    accepted.push(goal_json(&goal));
                }
            }
        }
    }
    Ok(Json(json!({
        "accepted": accepted,
        "refused": refused,
        "already_open": already_open,
    })))
}
