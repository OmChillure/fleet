//! Generalized durable cases: backend owns status/stage/wake; LLM decides at steps.
//!
//! `use_tool` calls `integration_query` on the control plane. `launch_agents`
//! accepts a plan and sets `cases.run_id`; the wake loop resumes when the run
//! is terminal. Coding fleets never receive OAuth tokens.

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use chrono::Utc;
use fleet_auth::{require_user, AppError, AppResult, AuthState};
use fleet_db::repos::cases as case_repo;
use fleet_db::repos::workspaces as workspace_repo;
use fleet_db::{Case, CaseEvent};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use fleet_integrations::proxy::ProxyCaller;
use fleet_orchestrator::domain::{PlanTask, WorkRunStatus};
use fleet_orchestrator::validate;

use crate::chat_tools::build_multi_task_plan;
use crate::state::AppState;

pub const MAX_DECISIONS_PER_CASE: i32 = 200;
pub const MAX_WAIT_SECS: i64 = 7 * 24 * 3600;
pub const DEFAULT_WAIT_SECS: i64 = 3600;
pub const CASE_WAKE_INTERVAL_S: u64 = 15;
const MAX_OBSERVATIONS: usize = 40;
const MAX_NOTE_CHARS: usize = 2000;
const STALE_DECIDING_SECS: i64 = 300;

// ── Status FSM ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaseStatus {
    Open,
    Waiting,
    Deciding,
    Acting,
    Completed,
    Failed,
    Cancelled,
}

impl CaseStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Waiting => "waiting",
            Self::Deciding => "deciding",
            Self::Acting => "acting",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "open" => Some(Self::Open),
            "waiting" => Some(Self::Waiting),
            "deciding" => Some(Self::Deciding),
            "acting" => Some(Self::Acting),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

static ALLOWED: LazyLock<HashMap<CaseStatus, Vec<CaseStatus>>> = LazyLock::new(|| {
    use CaseStatus::*;
    HashMap::from([
        (
            Open,
            vec![Waiting, Deciding, Acting, Completed, Failed, Cancelled],
        ),
        (Waiting, vec![Open, Deciding, Completed, Failed, Cancelled]),
        (
            Deciding,
            vec![Open, Waiting, Acting, Completed, Failed, Cancelled],
        ),
        (
            Acting,
            vec![Open, Waiting, Deciding, Completed, Failed, Cancelled],
        ),
        // terminals: no outbound
    ])
});

pub fn can_transition(from: CaseStatus, to: CaseStatus) -> bool {
    if from == to {
        return true; // idempotent same-status patch allowed for stage/context
    }
    if from.is_terminal() {
        return false;
    }
    ALLOWED.get(&from).map(|v| v.contains(&to)).unwrap_or(false)
}

// ── Decision schema ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct CaseDecision {
    pub action: String,
    #[serde(default)]
    pub stage: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub wait_seconds: Option<i64>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub launch: Option<Value>,
    #[serde(default)]
    pub tool: Option<Value>,
}

pub fn parse_decision_json(raw: &str) -> Result<CaseDecision, String> {
    let trimmed = raw.trim();
    let json_str = if let Some(start) = trimmed.find('{') {
        let end = trimmed
            .rfind('}')
            .ok_or_else(|| "no closing brace".to_string())?;
        &trimmed[start..=end]
    } else {
        return Err("decision is not JSON object".into());
    };
    serde_json::from_str(json_str).map_err(|e| format!("invalid decision JSON: {e}"))
}

pub fn decision_system_prompt() -> &'static str {
    "You are the Fleet case engine. You only output a single JSON object (no markdown).\n\
Schema:\n\
{\"action\":\"wait|note|set_stage|complete|fail|launch_agents|use_tool\",\n\
 \"stage\":\"optional free-form stage string\",\n\
 \"note\":\"optional fact to store\",\n\
 \"wait_seconds\":3600,\n\
 \"reason\":\"short\",\n\
 \"launch\":{\"title\":\"...\",\"tasks\":[{\"id\":\"t1\",\"title\":\"...\",\"spec\":\"...\",\"worker_profile\":\"generalist\"}]},\n\
 \"tool\":{\"provider\":\"...\",\"operation\":\"...\",\"args\":{}}}\n\
Rules:\n\
- Backend owns status. You only choose the next action.\n\
- Prefer wait when nothing new happened.\n\
- use_tool only if an integration can satisfy the policy; otherwise note/wait.\n\
- After a tool result or fleet run in context, decide the next step (do not repeat a succeeded write).\n\
- complete/fail when the policy goal is done or impossible.\n\
- stage is free-form (e.g. awaiting_reply, confirm_sent, booked).\n\
- Never invent secrets or claim external writes without tools."
}

// ── HTTP router ─────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/workspaces/{workspace_id}/cases",
            get(http_list).post(http_create),
        )
        .route(
            "/api/workspaces/{workspace_id}/cases/{case_id}",
            get(http_get),
        )
        .route(
            "/api/workspaces/{workspace_id}/cases/{case_id}/observe",
            post(http_observe),
        )
        .route(
            "/api/workspaces/{workspace_id}/cases/{case_id}/cancel",
            post(http_cancel),
        )
        .route(
            "/api/workspaces/{workspace_id}/cases/{case_id}/decide",
            post(http_decide_manual),
        )
        .route(
            "/api/workspaces/{workspace_id}/cases/{case_id}/wake",
            post(http_wake_now),
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

#[derive(Debug, Deserialize)]
pub struct CreateCaseBody {
    pub title: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub policy_md: Option<String>,
    #[serde(default)]
    pub stage: Option<String>,
    #[serde(default)]
    pub context: Option<Value>,
    /// Seconds until first wake (default 0 = wake immediately).
    #[serde(default)]
    pub wake_in_s: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    status: Option<String>,
    #[serde(default = "default_list_limit")]
    limit: i64,
}

fn default_list_limit() -> i64 {
    50
}

#[derive(Debug, Deserialize)]
pub struct ObserveBody {
    pub note: String,
    #[serde(default)]
    pub wake: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ManualDecideBody {
    pub decision: CaseDecision,
}

async fn http_create(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<CreateCaseBody>,
) -> AppResult<Json<Value>> {
    let (user, _) = require_member(&state, &jar, workspace_id).await?;
    let case = create_case(&state, workspace_id, Some(user.id), body).await?;
    Ok(Json(case_json(&case, None)))
}

async fn http_list(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let filter = q.status.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let rows = case_repo::list_for_workspace(&state.db, workspace_id, filter, q.limit)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(Json(json!({
        "cases": rows.iter().map(|c| case_json(c, None)).collect::<Vec<_>>(),
        "count": rows.len(),
    })))
}

async fn http_get(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((workspace_id, case_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let case = case_repo::get(&state.db, workspace_id, case_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
        .ok_or_else(|| AppError::not_found("case not found"))?;
    let events = case_repo::list_events(&state.db, case_id, 40)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(Json(case_json(&case, Some(&events))))
}

async fn http_observe(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((workspace_id, case_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<ObserveBody>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let case = observe_case(
        &state,
        workspace_id,
        case_id,
        &body.note,
        body.wake.unwrap_or(true),
    )
    .await?;
    Ok(Json(case_json(&case, None)))
}

async fn http_cancel(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((workspace_id, case_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let case = cancel_case(&state, workspace_id, case_id).await?;
    Ok(Json(case_json(&case, None)))
}

async fn http_decide_manual(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((workspace_id, case_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<ManualDecideBody>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let mut case = case_repo::get(&state.db, workspace_id, case_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
        .ok_or_else(|| AppError::not_found("case not found"))?;
    // Claim-like: move to deciding if not terminal
    let from = CaseStatus::parse(&case.status).unwrap_or(CaseStatus::Open);
    if from.is_terminal() {
        return Err(AppError::bad_request("case is terminal"));
    }
    if from != CaseStatus::Deciding {
        case = case_repo::apply_patch(
            &state.db,
            workspace_id,
            case_id,
            &case_repo::CasePatch {
                status: CaseStatus::Deciding.as_str().into(),
                stage: case.stage.clone(),
                context: case.context.clone(),
                next_wake_at: case.next_wake_at,
                last_error: None,
                run_id: None,
                clear_run_id: false,
                bump_decision: false,
                expected_version: case.version,
            },
        )
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    }
    let case = apply_decision(&state, &case, body.decision).await?;
    Ok(Json(case_json(&case, None)))
}

/// Run the case engine now (LLM or heuristic) — same path as the wake loop.
async fn http_wake_now(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((workspace_id, case_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<Value>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let case = case_repo::get(&state.db, workspace_id, case_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
        .ok_or_else(|| AppError::not_found("case not found"))?;
    let from = CaseStatus::parse(&case.status).unwrap_or(CaseStatus::Open);
    if from.is_terminal() {
        return Err(AppError::bad_request("case is terminal"));
    }
    let _ = case_repo::append_event(
        &state.db,
        case.id,
        case.workspace_id,
        "wake_requested",
        json!({ "source": "ui" }),
    )
    .await;
    let decision = decide_for_case(&state, &case).await;
    let case = apply_decision(&state, &case, decision).await?;
    Ok(Json(case_json(&case, None)))
}

// ── Service ─────────────────────────────────────────────────────────────────

pub fn case_json(case: &Case, events: Option<&[CaseEvent]>) -> Value {
    let mut v = json!({
        "id": case.id,
        "workspace_id": case.workspace_id,
        "kind": case.kind,
        "title": case.title,
        "policy_md": case.policy_md,
        "status": case.status,
        "stage": case.stage,
        "context": case.context,
        "next_wake_at": case.next_wake_at,
        "last_error": case.last_error,
        "run_id": case.run_id,
        "decision_count": case.decision_count,
        "created_by": case.created_by,
        "version": case.version,
        "created_at": case.created_at,
        "updated_at": case.updated_at,
    });
    if let Some(ev) = events {
        v["events"] = json!(ev
            .iter()
            .map(|e| json!({
                "id": e.id,
                "kind": e.kind,
                "payload": e.payload,
                "created_at": e.created_at,
            }))
            .collect::<Vec<_>>());
    }
    v
}

pub async fn create_case(
    state: &AppState,
    workspace_id: Uuid,
    created_by: Option<Uuid>,
    body: CreateCaseBody,
) -> AppResult<Case> {
    let title = body.title.trim().to_string();
    if title.is_empty() || title.len() > 300 {
        return Err(AppError::bad_request("title must be 1–300 chars"));
    }
    let kind = body
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("generic")
        .to_string();
    let policy_md = body.policy_md.unwrap_or_default();
    let stage = body
        .stage
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("start")
        .to_string();
    let context = body.context.unwrap_or_else(|| json!({}));
    let wake_in = body.wake_in_s.unwrap_or(0).max(0);
    let next_wake = Some(Utc::now() + chrono::Duration::seconds(wake_in));
    let id = Uuid::now_v7();
    let case = case_repo::insert(
        &state.db,
        &case_repo::NewCase {
            id,
            workspace_id,
            kind,
            title,
            policy_md,
            stage,
            context,
            next_wake_at: next_wake,
            created_by,
        },
    )
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;
    let _ = case_repo::append_event(
        &state.db,
        case.id,
        workspace_id,
        "created",
        json!({ "wake_in_s": wake_in }),
    )
    .await;
    Ok(case)
}

pub async fn observe_case(
    state: &AppState,
    workspace_id: Uuid,
    case_id: Uuid,
    note: &str,
    wake: bool,
) -> AppResult<Case> {
    let note = note.trim();
    if note.is_empty() {
        return Err(AppError::bad_request("note is required"));
    }
    if note.chars().count() > 4000 {
        return Err(AppError::bad_request("note too long (max 4000 chars)"));
    }
    let case = case_repo::get(&state.db, workspace_id, case_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
        .ok_or_else(|| AppError::not_found("case not found"))?;
    let status = CaseStatus::parse(&case.status).unwrap_or(CaseStatus::Open);
    if status.is_terminal() {
        return Err(AppError::bad_request("cannot observe a terminal case"));
    }
    let mut ctx = case.context.clone();
    push_observation(&mut ctx, note, "human");
    let patch = case_repo::CasePatch {
        status: if status == CaseStatus::Acting {
            case.status.clone()
        } else {
            CaseStatus::Open.as_str().into()
        },
        stage: case.stage.clone(),
        context: ctx,
        next_wake_at: if wake && status != CaseStatus::Acting {
            Some(Utc::now())
        } else {
            case.next_wake_at
        },
        last_error: None,
        run_id: None,
        clear_run_id: false,
        bump_decision: false,
        expected_version: case.version,
    };
    let updated = case_repo::apply_patch(&state.db, workspace_id, case_id, &patch)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let _ = case_repo::append_event(
        &state.db,
        case_id,
        workspace_id,
        "observed",
        json!({ "note": note, "wake": wake }),
    )
    .await;
    Ok(updated)
}

pub async fn cancel_case(state: &AppState, workspace_id: Uuid, case_id: Uuid) -> AppResult<Case> {
    let case = case_repo::get(&state.db, workspace_id, case_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
        .ok_or_else(|| AppError::not_found("case not found"))?;
    let from = CaseStatus::parse(&case.status).unwrap_or(CaseStatus::Open);
    if from.is_terminal() {
        return Ok(case);
    }
    if !can_transition(from, CaseStatus::Cancelled) {
        return Err(AppError::bad_request(format!(
            "cannot cancel from {}",
            case.status
        )));
    }
    let updated = case_repo::apply_patch(
        &state.db,
        workspace_id,
        case_id,
        &case_repo::CasePatch {
            status: CaseStatus::Cancelled.as_str().into(),
            stage: case.stage.clone(),
            context: case.context.clone(),
            next_wake_at: None,
            last_error: None,
            run_id: None,
            clear_run_id: false,
            bump_decision: false,
            expected_version: case.version,
        },
    )
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;
    let _ = case_repo::append_event(&state.db, case_id, workspace_id, "cancelled", json!({})).await;
    Ok(updated)
}

pub async fn apply_decision(
    state: &AppState,
    case: &Case,
    decision: CaseDecision,
) -> AppResult<Case> {
    let from = CaseStatus::parse(&case.status).unwrap_or(CaseStatus::Deciding);
    if from.is_terminal() {
        return Err(AppError::bad_request("case is terminal"));
    }
    if case.decision_count >= MAX_DECISIONS_PER_CASE {
        return fail_case(state, case, "max decisions exceeded").await;
    }

    let action = decision.action.trim().to_ascii_lowercase();
    let reason = decision.reason.clone().unwrap_or_default();
    let _ = case_repo::append_event(
        &state.db,
        case.id,
        case.workspace_id,
        "decided",
        json!({
            "action": action,
            "reason": reason,
            "stage": decision.stage,
            "note": decision.note,
            "wait_seconds": decision.wait_seconds,
        }),
    )
    .await;

    let mut stage = case.stage.clone();
    if let Some(s) = decision
        .stage
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        stage = s.to_string();
    }
    let mut ctx = case.context.clone();
    if !ctx.is_object() {
        ctx = json!({});
    }
    if let Some(note) = decision
        .note
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        push_observation(&mut ctx, note, "decision");
        let _ = case_repo::append_event(
            &state.db,
            case.id,
            case.workspace_id,
            "noted",
            json!({ "note": note }),
        )
        .await;
    }
    if stage != case.stage {
        let _ = case_repo::append_event(
            &state.db,
            case.id,
            case.workspace_id,
            "stage",
            json!({ "from": case.stage, "to": stage }),
        )
        .await;
    }

    match action.as_str() {
        "wait" | "note" | "set_stage" => {
            let wait = decision
                .wait_seconds
                .unwrap_or(DEFAULT_WAIT_SECS)
                .clamp(5, MAX_WAIT_SECS);
            let next = Utc::now() + chrono::Duration::seconds(wait);
            let to = CaseStatus::Waiting;
            if !can_transition(from, to) {
                return Err(AppError::bad_request(format!(
                    "illegal transition {} → {}",
                    from.as_str(),
                    to.as_str()
                )));
            }
            let updated = case_repo::apply_patch(
                &state.db,
                case.workspace_id,
                case.id,
                &case_repo::CasePatch {
                    status: to.as_str().into(),
                    stage,
                    context: ctx,
                    next_wake_at: Some(next),
                    last_error: None,
                    run_id: None,
                    clear_run_id: false,
                    bump_decision: true,
                    expected_version: case.version,
                },
            )
            .await
            .map_err(|e| AppError::internal(e.to_string()))?;
            let _ = case_repo::append_event(
                &state.db,
                case.id,
                case.workspace_id,
                "waited",
                json!({ "wait_seconds": wait, "next_wake_at": next }),
            )
            .await;
            Ok(updated)
        }
        "complete" => {
            let updated = case_repo::apply_patch(
                &state.db,
                case.workspace_id,
                case.id,
                &case_repo::CasePatch {
                    status: CaseStatus::Completed.as_str().into(),
                    stage,
                    context: ctx,
                    next_wake_at: None,
                    last_error: None,
                    run_id: None,
                    clear_run_id: false,
                    bump_decision: true,
                    expected_version: case.version,
                },
            )
            .await
            .map_err(|e| AppError::internal(e.to_string()))?;
            let _ = case_repo::append_event(
                &state.db,
                case.id,
                case.workspace_id,
                "completed",
                json!({ "reason": reason }),
            )
            .await;
            Ok(updated)
        }
        "fail" => fail_case_with(state, case, stage, ctx, &reason).await,
        "use_tool" => apply_use_tool(state, case, decision, stage, ctx).await,
        "launch_agents" => apply_launch_agents(state, case, decision, stage, ctx).await,
        other => {
            let _ = case_repo::append_event(
                &state.db,
                case.id,
                case.workspace_id,
                "error",
                json!({ "error": format!("unknown action `{other}`") }),
            )
            .await;
            // Recover to waiting
            let next = Utc::now() + chrono::Duration::seconds(DEFAULT_WAIT_SECS);
            let updated = case_repo::apply_patch(
                &state.db,
                case.workspace_id,
                case.id,
                &case_repo::CasePatch {
                    status: CaseStatus::Waiting.as_str().into(),
                    stage: case.stage.clone(),
                    context: case.context.clone(),
                    next_wake_at: Some(next),
                    last_error: Some(format!("unknown action `{other}`")),
                    run_id: None,
                    clear_run_id: false,
                    bump_decision: true,
                    expected_version: case.version,
                },
            )
            .await
            .map_err(|e| AppError::internal(e.to_string()))?;
            Ok(updated)
        }
    }
}

async fn fail_case(state: &AppState, case: &Case, reason: &str) -> AppResult<Case> {
    fail_case_with(
        state,
        case,
        case.stage.clone(),
        case.context.clone(),
        reason,
    )
    .await
}

async fn fail_case_with(
    state: &AppState,
    case: &Case,
    stage: String,
    context: Value,
    reason: &str,
) -> AppResult<Case> {
    let updated = case_repo::apply_patch(
        &state.db,
        case.workspace_id,
        case.id,
        &case_repo::CasePatch {
            status: CaseStatus::Failed.as_str().into(),
            stage,
            context,
            next_wake_at: None,
            last_error: Some(reason.to_string()),
            run_id: None,
            clear_run_id: false,
            bump_decision: true,
            expected_version: case.version,
        },
    )
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;
    let _ = case_repo::append_event(
        &state.db,
        case.id,
        case.workspace_id,
        "failed",
        json!({ "reason": reason }),
    )
    .await;
    Ok(updated)
}

/// `CASE_AUTO_DECIDE` unset / on applies decide. `off` / `0` / `false` / `no` skip it.
fn case_auto_decide_enabled(raw: Option<&str>) -> bool {
    match raw {
        None => true,
        Some(v) => {
            let t = v.trim().to_ascii_lowercase();
            !(t == "0" || t == "false" || t == "off" || t == "no")
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessDueClaimAction {
    /// LLM / heuristic `decide_for_case` + `apply_decision`.
    ApplyDecide,
    /// Return the claimed case to waiting; do not decide or bump `decision_count`.
    UnclaimWaiting,
}

fn process_due_claim_action(auto_decide: bool) -> ProcessDueClaimAction {
    if auto_decide {
        ProcessDueClaimAction::ApplyDecide
    } else {
        ProcessDueClaimAction::UnclaimWaiting
    }
}

fn auto_decide_off_unclaim_patch(case: &Case) -> case_repo::CasePatch {
    let next = case
        .next_wake_at
        .unwrap_or_else(|| Utc::now() + chrono::Duration::seconds(DEFAULT_WAIT_SECS));
    case_repo::CasePatch {
        status: CaseStatus::Waiting.as_str().into(),
        stage: case.stage.clone(),
        context: case.context.clone(),
        next_wake_at: Some(next),
        last_error: Some("CASE_AUTO_DECIDE=off".into()),
        run_id: None,
        clear_run_id: false,
        bump_decision: false,
        expected_version: case.version,
    }
}

/// Claim already set `deciding`. A failed unclaim must retry waiting — never decide.
fn process_due_after_unclaim_failure(case: &Case) -> (ProcessDueClaimAction, case_repo::CasePatch) {
    (
        ProcessDueClaimAction::UnclaimWaiting,
        auto_decide_off_unclaim_patch(case),
    )
}

/// Background: claim due cases and apply a heuristic or LLM decision.
///
/// Claims one case at a time so a slow LLM decide cannot leave the rest of
/// the batch stuck in `deciding` for the duration of the queue.
pub async fn process_due(state: &AppState, limit: i64) {
    match case_repo::reclaim_stale_deciding(&state.db, STALE_DECIDING_SECS).await {
        Ok(n) if n > 0 => {
            tracing::warn!(count = n, "reclaimed stale deciding cases");
        }
        Err(e) => tracing::warn!(error = %e, "reclaim_stale_deciding failed"),
        _ => {}
    }
    reconcile_acting_runs(state).await;

    let auto = case_auto_decide_enabled(std::env::var("CASE_AUTO_DECIDE").ok().as_deref());

    let limit = limit.clamp(1, 20);
    for _ in 0..limit {
        let due = match case_repo::claim_due(&state.db, 1).await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %e, "claim_due cases failed");
                break;
            }
        };
        let Some(case) = due.into_iter().next() else {
            break;
        };
        if process_due_claim_action(auto) == ProcessDueClaimAction::UnclaimWaiting {
            let patch = auto_decide_off_unclaim_patch(&case);
            if case_repo::apply_patch(&state.db, case.workspace_id, case.id, &patch)
                .await
                .is_err()
            {
                let (action, retry) = process_due_after_unclaim_failure(&case);
                debug_assert_eq!(action, ProcessDueClaimAction::UnclaimWaiting);
                if let Err(e) =
                    case_repo::apply_patch(&state.db, case.workspace_id, case.id, &retry).await
                {
                    tracing::warn!(
                        case_id = %case.id,
                        error = %e,
                        "CASE_AUTO_DECIDE=off unclaim failed; leaving decide unapplied"
                    );
                }
            }
            continue;
        }
        let _ = case_repo::append_event(
            &state.db,
            case.id,
            case.workspace_id,
            "woke",
            json!({ "decision_count": case.decision_count }),
        )
        .await;
        let decision = decide_for_case(state, &case).await;
        if let Err(e) = apply_decision(state, &case, decision).await {
            tracing::warn!(
                case_id = %case.id,
                error = %e,
                "case apply_decision failed"
            );
            let next = Utc::now() + chrono::Duration::seconds(DEFAULT_WAIT_SECS);
            let _ = case_repo::apply_patch(
                &state.db,
                case.workspace_id,
                case.id,
                &case_repo::CasePatch {
                    status: CaseStatus::Waiting.as_str().into(),
                    stage: case.stage.clone(),
                    context: case.context.clone(),
                    next_wake_at: Some(next),
                    last_error: Some(e.to_string()),
                    run_id: None,
                    clear_run_id: false,
                    bump_decision: false,
                    expected_version: case.version,
                },
            )
            .await;
        }
    }
}

/// Heuristic decision (no LLM required). Prefer wait; complete only on explicit context flag.
pub fn heuristic_decision(case: &Case) -> CaseDecision {
    let observations = case
        .context
        .get("observations")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let complete_hint = case
        .context
        .get("force_complete")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if complete_hint {
        return CaseDecision {
            action: "complete".into(),
            stage: Some("done".into()),
            note: Some("force_complete in context".into()),
            wait_seconds: None,
            reason: Some("context.force_complete".into()),
            launch: None,
            tool: None,
        };
    }
    // If policy empty and no observations, wait long; if observations, set_stage progressing
    if observations > 0 {
        CaseDecision {
            action: "set_stage".into(),
            stage: Some(format!("observed_{observations}")),
            note: None,
            wait_seconds: Some(DEFAULT_WAIT_SECS),
            reason: Some("new observations; waiting for more or human complete".into()),
            launch: None,
            tool: None,
        }
    } else {
        CaseDecision {
            action: "wait".into(),
            stage: None,
            note: None,
            wait_seconds: Some(DEFAULT_WAIT_SECS),
            reason: Some("no new facts".into()),
            launch: None,
            tool: None,
        }
    }
}

/// `CASE_LLM_DECIDE=0` forces the heuristic. Unset / on uses Grok or Ollama
/// when that provider is enabled, else heuristic.
pub fn llm_decide_wanted(grok_enabled: bool, ollama_enabled: bool) -> bool {
    match std::env::var("CASE_LLM_DECIDE") {
        Ok(v) => {
            let t = v.trim().to_ascii_lowercase();
            t == "1" || t == "true" || t == "on" || t == "yes"
        }
        Err(_) => grok_enabled || ollama_enabled,
    }
}

async fn decide_for_case(state: &AppState, case: &Case) -> CaseDecision {
    if !llm_decide_wanted(state.grok.enabled, state.ollama.enabled) {
        return heuristic_decision(case);
    }
    match llm_decision(state, case).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(case_id = %case.id, error = %e, "case LLM decide failed; heuristic");
            heuristic_decision(case)
        }
    }
}

async fn llm_decision(state: &AppState, case: &Case) -> Result<CaseDecision, String> {
    let events = case_repo::list_events(&state.db, case.id, 12)
        .await
        .unwrap_or_default();
    let user = case_decision_user_prompt(case, &events);
    let system = decision_system_prompt();

    let raw = if state.grok.enabled {
        crate::grok::complete_text(&state.grok, &crate::grok::default_model(), system, &user)
            .await
            .map_err(|e| e.to_string())?
    } else if state.ollama.enabled {
        let model = state
            .ollama
            .default_model
            .clone()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| "OLLAMA_MODEL is unset".to_string())?;
        crate::chat::complete_text(&state.ollama, &model, system, &user)
            .await
            .map_err(|e| e.to_string())?
    } else {
        return Err("no chat LLM configured".into());
    };
    parse_decision_json(&raw)
}

fn case_decision_user_prompt(case: &Case, events: &[CaseEvent]) -> String {
    let ev: Vec<Value> = events
        .iter()
        .map(|e| {
            json!({
                "kind": e.kind,
                "payload": e.payload,
                "at": e.created_at,
            })
        })
        .collect();
    let mut snap = json!({
        "id": case.id,
        "title": case.title,
        "kind": case.kind,
        "policy_md": case.policy_md,
        "status": case.status,
        "stage": case.stage,
        "context": case.context,
        "run_id": case.run_id,
        "decision_count": case.decision_count,
        "last_error": case.last_error,
        "recent_events": ev,
    });
    let s = snap.to_string();
    if s.len() > 12_000 {
        if let Some(ctx) = snap.get_mut("context") {
            *ctx = json!("(truncated)");
        }
    }
    format!("Decide the next action for this case. Output JSON only.\n{snap}")
}

/// When a linked fleet run finishes, wake the case so the LLM can PR / Notion / complete.
async fn reconcile_acting_runs(state: &AppState) {
    let acting = match case_repo::list_acting_with_run(&state.db, 20).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "list_acting_with_run failed");
            return;
        }
    };
    for case in acting {
        let Some(run_id) = case.run_id else {
            continue;
        };
        let run = match state.orch.store.get_run(case.workspace_id, run_id).await {
            Ok(Some(r)) => r,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(case_id = %case.id, error = %e, "get_run for acting case failed");
                continue;
            }
        };
        let Some(st) = WorkRunStatus::from_db(&run.status) else {
            continue;
        };
        if !st.is_terminal() {
            continue;
        }
        let snap = state
            .orch
            .store
            .load_board_snapshot(case.workspace_id)
            .await
            .ok();
        let task_summaries: Vec<Value> = snap
            .as_ref()
            .map(|s| {
                s.tasks
                    .iter()
                    .filter(|t| t.run_id == run_id)
                    .map(|t| {
                        json!({
                            "id": t.plan_task_id,
                            "title": t.title,
                            "status": t.status,
                            "profile": t.worker_profile,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let files = {
            let data_dir = state.orch.tick.data_dir();
            let ws = fleet_orchestrator::sandbox::share::run_workspace_dir(
                data_dir,
                case.workspace_id,
                run_id,
            );
            fleet_orchestrator::orch::list_shared_files(&ws)
        };
        let mut ctx = case.context.clone();
        if !ctx.is_object() {
            ctx = json!({});
        }
        if let Some(obj) = ctx.as_object_mut() {
            obj.insert(
                "fleet_run".into(),
                json!({
                    "id": run_id,
                    "status": run.status,
                    "tasks": task_summaries,
                    "files": files,
                }),
            );
        }
        push_observation(
            &mut ctx,
            &format!("fleet run {run_id} ended with status {}", run.status),
            "fleet",
        );
        let _ = case_repo::append_event(
            &state.db,
            case.id,
            case.workspace_id,
            "fleet_terminal",
            json!({ "run_id": run_id, "status": run.status }),
        )
        .await;
        let _ = case_repo::apply_patch(
            &state.db,
            case.workspace_id,
            case.id,
            &case_repo::CasePatch {
                status: CaseStatus::Waiting.as_str().into(),
                stage: case.stage.clone(),
                context: ctx,
                next_wake_at: Some(Utc::now()),
                last_error: None,
                run_id: None,
                clear_run_id: false,
                bump_decision: false,
                expected_version: case.version,
            },
        )
        .await;
    }
}

pub fn parse_tool_request(tool: &Value) -> Result<(String, String, Value), String> {
    let provider = tool
        .get("provider")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "tool.provider is required".to_string())?;
    let operation = tool
        .get("operation")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "tool.operation is required".to_string())?;
    let args = tool.get("args").cloned().unwrap_or_else(|| json!({}));
    Ok((provider, operation, args))
}

fn push_observation(ctx: &mut Value, note: &str, from: &str) {
    if !ctx.is_object() {
        *ctx = json!({});
    }
    let note = {
        let t = note.trim();
        if t.chars().count() > MAX_NOTE_CHARS {
            let cut: String = t.chars().take(MAX_NOTE_CHARS).collect();
            format!("{cut}…")
        } else {
            t.to_string()
        }
    };
    let Some(obj) = ctx.as_object_mut() else {
        return;
    };
    let observations = obj.entry("observations").or_insert_with(|| json!([]));
    let Some(arr) = observations.as_array_mut() else {
        return;
    };
    let mut entry = json!({
        "at": Utc::now().to_rfc3339(),
        "note": note,
    });
    if !from.is_empty() && from != "human" {
        entry["from"] = json!(from);
    }
    arr.push(entry);
    if arr.len() > MAX_OBSERVATIONS {
        let skip = arr.len() - MAX_OBSERVATIONS;
        *arr = arr.iter().skip(skip).cloned().collect();
    }
}

fn truncate_json(v: &Value, max: usize) -> Value {
    let s = v.to_string();
    if s.len() <= max {
        return v.clone();
    }
    json!({ "_truncated": true, "preview": s.chars().take(max).collect::<String>() })
}

fn push_tool_result(ctx: &mut Value, entry: Value) {
    if !ctx.is_object() {
        *ctx = json!({});
    }
    if let Some(obj) = ctx.as_object_mut() {
        obj.insert("last_tool_result".into(), entry.clone());
        let log = obj.entry("tool_results").or_insert_with(|| json!([]));
        if let Some(arr) = log.as_array_mut() {
            arr.push(entry);
            if arr.len() > 8 {
                let skip = arr.len() - 8;
                *arr = arr.iter().skip(skip).cloned().collect();
            }
        }
    }
}

async fn apply_use_tool(
    state: &AppState,
    case: &Case,
    decision: CaseDecision,
    stage: String,
    mut ctx: Value,
) -> AppResult<Case> {
    let tool = decision.tool.clone().unwrap_or_else(|| json!({}));
    let parsed = parse_tool_request(&tool);
    let (ok, summary, stored, last_error, wait) = match parsed {
        Err(e) => (
            false,
            e.clone(),
            json!({ "ok": false, "error": e }),
            Some(e),
            60i64,
        ),
        Ok((provider, operation, args)) => {
            let result = crate::workforce::gated_invoke(
                state,
                case.workspace_id,
                match case.created_by {
                    Some(user_id) => ProxyCaller::Chat { user_id },
                    None => ProxyCaller::System,
                },
                &provider,
                &operation,
                args,
            )
            .await;
            let stored = json!({
                "at": Utc::now().to_rfc3339(),
                "ok": result.ok,
                "provider": result.provider,
                "operation": result.operation,
                "summary": result.summary,
                "error_code": result.error_code,
                "data": truncate_json(&result.data, 4000),
            });
            let wait = if result.ok { 15 } else { 120 };
            let err = if result.ok {
                None
            } else {
                Some(result.summary.clone())
            };
            (result.ok, result.summary, stored, err, wait)
        }
    };

    push_tool_result(&mut ctx, stored.clone());
    let _ = case_repo::append_event(
        &state.db,
        case.id,
        case.workspace_id,
        if ok { "tool_ok" } else { "tool_error" },
        stored,
    )
    .await;

    let wait = decision
        .wait_seconds
        .unwrap_or(wait)
        .clamp(5, MAX_WAIT_SECS);
    let next = Utc::now() + chrono::Duration::seconds(wait);
    let updated = case_repo::apply_patch(
        &state.db,
        case.workspace_id,
        case.id,
        &case_repo::CasePatch {
            status: CaseStatus::Waiting.as_str().into(),
            stage,
            context: ctx,
            next_wake_at: Some(next),
            last_error,
            run_id: None,
            clear_run_id: false,
            bump_decision: true,
            expected_version: case.version,
        },
    )
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;
    let _ = summary;
    Ok(updated)
}

async fn apply_launch_agents(
    state: &AppState,
    case: &Case,
    decision: CaseDecision,
    stage: String,
    mut ctx: Value,
) -> AppResult<Case> {
    let launch = decision.launch.clone().unwrap_or_else(|| json!({}));
    match launch_case_plan(state, case, &launch).await {
        Ok(run_id) => {
            if let Some(obj) = ctx.as_object_mut() {
                obj.insert("pending_launch".into(), launch.clone());
                obj.insert(
                    "fleet_run".into(),
                    json!({ "id": run_id, "status": "accepted" }),
                );
            }
            let _ = case_repo::append_event(
                &state.db,
                case.id,
                case.workspace_id,
                "agents_launched",
                json!({ "run_id": run_id, "launch": launch }),
            )
            .await;
            case_repo::apply_patch(
                &state.db,
                case.workspace_id,
                case.id,
                &case_repo::CasePatch {
                    status: CaseStatus::Acting.as_str().into(),
                    stage,
                    context: ctx,
                    next_wake_at: None,
                    last_error: None,
                    run_id: Some(run_id),
                    clear_run_id: false,
                    bump_decision: true,
                    expected_version: case.version,
                },
            )
            .await
            .map_err(|e| AppError::internal(e.to_string()))
        }
        Err(e) => {
            if let Some(obj) = ctx.as_object_mut() {
                obj.insert("pending_launch".into(), launch.clone());
            }
            let _ = case_repo::append_event(
                &state.db,
                case.id,
                case.workspace_id,
                "agents_launch_failed",
                json!({ "error": e, "launch": launch }),
            )
            .await;
            let next = Utc::now() + chrono::Duration::seconds(60);
            case_repo::apply_patch(
                &state.db,
                case.workspace_id,
                case.id,
                &case_repo::CasePatch {
                    status: CaseStatus::Waiting.as_str().into(),
                    stage,
                    context: ctx,
                    next_wake_at: Some(next),
                    last_error: Some(e),
                    run_id: None,
                    clear_run_id: false,
                    bump_decision: true,
                    expected_version: case.version,
                },
            )
            .await
            .map_err(|e| AppError::internal(e.to_string()))
        }
    }
}

async fn launch_case_plan(state: &AppState, case: &Case, launch: &Value) -> Result<Uuid, String> {
    let title = launch
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(case.title.as_str());
    let tasks_in = launch
        .get("tasks")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut tasks: Vec<PlanTask> = Vec::new();
    if tasks_in.is_empty() {
        let spec = launch
            .get("spec")
            .and_then(|v| v.as_str())
            .unwrap_or(case.policy_md.as_str());
        let profile = launch
            .get("worker_profile")
            .and_then(|v| v.as_str())
            .unwrap_or("generalist");
        tasks.push(PlanTask {
            plan_task_id: "t1".into(),
            title: title.to_string(),
            summary: spec.chars().take(280).collect(),
            spec_md: spec.to_string(),
            depends_on: vec![],
            worker_profile: profile.to_string(),
            acceptance: fleet_orchestrator::orch::default_acceptance(profile),
            priority: 100,
            external_ref: None,
            timeout_s: Some(3600),
            max_retries: Some(1),
        });
    } else {
        for (i, t) in tasks_in.iter().enumerate() {
            let id = t
                .get("id")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("t{}", i + 1));
            let t_title = t
                .get("title")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(title);
            let spec = t.get("spec").and_then(|v| v.as_str()).unwrap_or("");
            let profile = t
                .get("worker_profile")
                .and_then(|v| v.as_str())
                .unwrap_or("generalist");
            let depends = t
                .get("depends_on")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            tasks.push(PlanTask {
                plan_task_id: id,
                title: t_title.to_string(),
                summary: spec.chars().take(280).collect(),
                spec_md: spec.to_string(),
                depends_on: depends,
                worker_profile: profile.to_string(),
                acceptance: fleet_orchestrator::orch::default_acceptance(profile),
                priority: t.get("priority").and_then(|v| v.as_i64()).unwrap_or(100) as i32,
                external_ref: None,
                timeout_s: Some(3600),
                max_retries: Some(1),
            });
        }
    }
    if tasks.is_empty() {
        return Err("launch_agents: no tasks".into());
    }

    let mut plan = build_multi_task_plan(case.workspace_id, None, tasks);
    plan.source = "case".into();
    let run_mode = workspace_repo::default_run_mode(&state.db, case.workspace_id)
        .await
        .unwrap_or_else(|_| fleet_db::WORKER_HOST_RUN_ON_PC.to_string());
    plan.policy.run_mode = run_mode.clone();
    let llm = crate::chat_tools::composer_or_fallback_for(
        state,
        case.workspace_id,
        case.created_by,
    )
    .await;
    if run_mode != fleet_db::WORKER_HOST_RUN_IN_CONTAINER {
        crate::chat_tools::require_taking_runner(&llm).map_err(|e| e.to_string())?;
    }
    crate::chat_tools::stamp_plan_llm(&mut plan, &llm);
    plan.policy.requested_by = case.created_by;
    validate::validate_plan(&plan).map_err(|e| e.to_string())?;

    state
        .orch
        .store
        .ensure_project(case.workspace_id, "workspace fleet", 3)
        .await
        .map_err(|e| e.to_string())?;
    let (run, _) = state
        .orch
        .store
        .insert_accepted_plan(case.workspace_id, plan)
        .await
        .map_err(|e| e.to_string())?;
    let _ = crate::workforce::attach_run_to_goal(
        state,
        case.workspace_id,
        case.created_by,
        &case.title,
        &case.policy_md,
        run.id,
        Some(case.id),
    )
    .await;
    Ok(run.id)
}

pub fn wake_interval() -> Duration {
    let secs = std::env::var("CASE_WAKE_INTERVAL_S")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(CASE_WAKE_INTERVAL_S);
    Duration::from_secs(secs.max(5))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transition_matrix_basics() {
        use CaseStatus::*;
        let cells: &[(CaseStatus, CaseStatus, bool)] = &[
            (Open, Open, true),
            (Open, Waiting, true),
            (Open, Deciding, true),
            (Open, Acting, true),
            (Open, Completed, true),
            (Open, Failed, true),
            (Open, Cancelled, true),
            (Waiting, Open, true),
            (Waiting, Waiting, true),
            (Waiting, Deciding, true),
            (Waiting, Acting, false),
            (Waiting, Completed, true),
            (Waiting, Failed, true),
            (Waiting, Cancelled, true),
            (Deciding, Open, true),
            (Deciding, Waiting, true),
            (Deciding, Deciding, true),
            (Deciding, Acting, true),
            (Deciding, Completed, true),
            (Deciding, Failed, true),
            (Deciding, Cancelled, true),
        ];
        for &(from, to, allowed) in cells {
            assert_eq!(
                can_transition(from, to),
                allowed,
                "{from:?} → {to:?}"
            );
        }
    }

    #[test]
    fn acting_can_transition_to_completed_failed_cancelled_waiting_deciding_open_and_self() {
        assert!(can_transition(CaseStatus::Acting, CaseStatus::Completed));
        assert!(can_transition(CaseStatus::Acting, CaseStatus::Failed));
        assert!(can_transition(CaseStatus::Acting, CaseStatus::Cancelled));
        assert!(can_transition(CaseStatus::Acting, CaseStatus::Waiting));
        assert!(can_transition(CaseStatus::Acting, CaseStatus::Deciding));
        assert!(can_transition(CaseStatus::Acting, CaseStatus::Open));
        assert!(can_transition(CaseStatus::Acting, CaseStatus::Acting));
    }

    #[test]
    fn is_terminal_only_completed_failed_cancelled() {
        assert!(!CaseStatus::Open.is_terminal());
        assert!(!CaseStatus::Waiting.is_terminal());
        assert!(!CaseStatus::Deciding.is_terminal());
        assert!(!CaseStatus::Acting.is_terminal());
        assert!(CaseStatus::Completed.is_terminal());
        assert!(CaseStatus::Failed.is_terminal());
        assert!(CaseStatus::Cancelled.is_terminal());
    }

    #[test]
    fn parse_decision_strips_fences() {
        let raw = "```json\n{\"action\":\"wait\",\"wait_seconds\":10,\"reason\":\"ok\"}\n```";
        let d = parse_decision_json(raw).expect("parse");
        assert_eq!(d.action, "wait");
        assert_eq!(d.wait_seconds, Some(10));
    }

    #[test]
    fn heuristic_waits_without_facts() {
        let case = Case {
            id: Uuid::nil(),
            workspace_id: Uuid::nil(),
            kind: "generic".into(),
            title: "t".into(),
            policy_md: "p".into(),
            status: "deciding".into(),
            stage: "start".into(),
            context: json!({}),
            next_wake_at: None,
            last_error: None,
            run_id: None,
            decision_count: 0,
            created_by: None,
            version: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let d = heuristic_decision(&case);
        assert_eq!(d.action, "wait");
    }

    #[test]
    fn heuristic_completes_on_flag() {
        let case = Case {
            id: Uuid::nil(),
            workspace_id: Uuid::nil(),
            kind: "generic".into(),
            title: "t".into(),
            policy_md: "p".into(),
            status: "deciding".into(),
            stage: "start".into(),
            context: json!({ "force_complete": true }),
            next_wake_at: None,
            last_error: None,
            run_id: None,
            decision_count: 0,
            created_by: None,
            version: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let d = heuristic_decision(&case);
        assert_eq!(d.action, "complete");
    }

    #[test]
    fn observations_are_capped() {
        let mut ctx = json!({});
        for i in 0..60 {
            push_observation(&mut ctx, &format!("note {i}"), "test");
        }
        let arr = ctx["observations"].as_array().expect("arr");
        assert_eq!(arr.len(), MAX_OBSERVATIONS);
        assert!(arr[0]["note"].as_str().unwrap().contains("note 20"));
        assert!(arr.last().unwrap()["note"]
            .as_str()
            .unwrap()
            .contains("note 59"));
    }

    #[test]
    fn parse_tool_request_requires_provider_and_op() {
        let (p, o, args) = parse_tool_request(&json!({
            "provider": "Slack",
            "operation": "get_channel_history",
            "args": { "channel": "C1" }
        }))
        .expect("ok");
        assert_eq!(p, "slack");
        assert_eq!(o, "get_channel_history");
        assert_eq!(args["channel"], "C1");
    }

    #[test]
    fn parse_tool_request_err_when_operation_missing_but_provider_present() {
        assert!(parse_tool_request(&json!({ "provider": "slack" })).is_err());
    }

    #[test]
    fn parse_tool_request_err_when_provider_missing_but_operation_present() {
        assert!(parse_tool_request(&json!({ "operation": "list_channels" })).is_err());
    }

    #[test]
    fn parse_tool_request_trims_and_lowercases_operation_defaults_missing_args() {
        let (p, o, args) = parse_tool_request(&json!({
            "provider": " Slack ",
            "operation": "Get_Channel_History"
        }))
        .expect("ok");
        assert_eq!(p, "slack");
        assert_eq!(o, "get_channel_history");
        assert_eq!(args, json!({}));
    }

    fn deciding_case(decision_count: i32, version: i64) -> Case {
        Case {
            id: Uuid::nil(),
            workspace_id: Uuid::nil(),
            kind: "generic".into(),
            title: "t".into(),
            policy_md: "p".into(),
            status: "deciding".into(),
            stage: "start".into(),
            context: json!({ "force_complete": true }),
            next_wake_at: Some(Utc::now()),
            last_error: None,
            run_id: None,
            decision_count,
            created_by: None,
            version,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn case_auto_decide_off_0_false_does_not_apply_decide() {
        for raw in ["off", "0", "false", "no", "OFF", " False ", " 0 "] {
            assert!(
                !case_auto_decide_enabled(Some(raw)),
                "CASE_AUTO_DECIDE={raw:?} must disable auto-decide"
            );
        }
    }

    #[test]
    fn case_auto_decide_blank_or_unknown_stays_enabled() {
        for raw in ["", "yes", "true", "maybe"] {
            assert!(
                case_auto_decide_enabled(Some(raw)),
                "CASE_AUTO_DECIDE={raw:?} must stay enabled (fail-open)"
            );
        }
    }

    #[test]
    fn case_auto_decide_off_unclaim_does_not_bump_decision_count() {
        let case = deciding_case(7, 3);
        let patch = auto_decide_off_unclaim_patch(&case);
        assert!(!patch.bump_decision);
        assert_eq!(patch.next_wake_at, case.next_wake_at);
    }

    #[test]
    fn auto_decide_off_unclaim_patch_nil_wake_schedules_default_wait() {
        let mut case = deciding_case(0, 1);
        case.next_wake_at = None;
        let before = Utc::now();
        let patch = auto_decide_off_unclaim_patch(&case);
        let after = Utc::now();
        let wake = patch.next_wake_at.expect("off unclaim schedules a wake");
        assert!(
            wake >= before + chrono::Duration::seconds(DEFAULT_WAIT_SECS)
                && wake <= after + chrono::Duration::seconds(DEFAULT_WAIT_SECS)
        );
    }
}
