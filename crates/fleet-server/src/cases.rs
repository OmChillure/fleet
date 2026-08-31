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

use fleet_integrations::proxy::{self, ProxyCaller, ProxyRequest};
use fleet_orchestrator::domain::{PlanTask, WorkPlanBody, WorkRunStatus};
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
const LAUNCH_FAIL_RETRY_SECS: i64 = 60;

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
        return Err(AppError::conflict("case is terminal"));
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

/// Terminals 400. Acting stays on reconcile — do not decide a live run.
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
    if from == CaseStatus::Acting {
        return Err(AppError::conflict("run still in progress"));
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
    if policy_md.len() > 32_000 {
        return Err(AppError::bad_request("policy_md too long (max 32k bytes)"));
    }
    let stage = body
        .stage
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("start")
        .to_string();
    let context = body.context.unwrap_or_else(|| json!({}));
    // Create allows 0 (immediate first wake). Cap matches decide `wait_seconds`
    // so 10^12 cannot overflow chrono or schedule year 33715.
    let wake_in = body.wake_in_s.unwrap_or(0).clamp(0, MAX_WAIT_SECS);
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
    let patch = observe_case_patch(&case, note, wake);
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

/// Acting must keep `run_id` so reconcile still owns the live fleet run.
fn observe_case_patch(case: &Case, note: &str, wake: bool) -> case_repo::CasePatch {
    let status = CaseStatus::parse(&case.status).unwrap_or(CaseStatus::Open);
    let mut ctx = case.context.clone();
    push_observation(&mut ctx, note, "human");
    case_repo::CasePatch {
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
    }
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
    let updated =
        case_repo::apply_patch(&state.db, workspace_id, case_id, &cancel_case_patch(&case))
            .await
            .map_err(|e| AppError::internal(e.to_string()))?;
    case_repo::append_event(&state.db, case_id, workspace_id, "cancelled", json!({}))
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(updated)
}

/// Cancel must NULL run_id so reconcile cannot revive the case.
fn cancel_case_patch(case: &Case) -> case_repo::CasePatch {
    case_repo::CasePatch {
        status: CaseStatus::Cancelled.as_str().into(),
        stage: case.stage.clone(),
        context: case.context.clone(),
        next_wake_at: None,
        last_error: None,
        run_id: None,
        clear_run_id: true,
        bump_decision: false,
        expected_version: case.version,
    }
}

/// Next decide/wake must fail the case; do not stay deciding/acting past the cap.
fn max_decisions_fail_reason(decision_count: i32) -> Option<&'static str> {
    if decision_count >= MAX_DECISIONS_PER_CASE {
        Some("max decisions exceeded")
    } else {
        None
    }
}

pub async fn apply_decision(
    state: &AppState,
    case: &Case,
    decision: CaseDecision,
) -> AppResult<Case> {
    let from = CaseStatus::parse(&case.status).unwrap_or(CaseStatus::Deciding);
    if from.is_terminal() {
        return Err(AppError::conflict("case is terminal"));
    }
    if let Some(reason) = max_decisions_fail_reason(case.decision_count) {
        return fail_case(state, case, reason).await;
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
                &complete_case_patch(case, stage, ctx),
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

/// Manual / heuristic `complete` is terminal. Clears `next_wake_at` so a
/// leftover due wake (acting after launch, or the decide-claim preserve)
/// cannot stay scheduled after the row is finished.
fn complete_case_patch(case: &Case, stage: String, context: Value) -> case_repo::CasePatch {
    case_repo::CasePatch {
        status: CaseStatus::Completed.as_str().into(),
        stage,
        context,
        next_wake_at: None,
        last_error: None,
        run_id: None,
        clear_run_id: false,
        bump_decision: true,
        expected_version: case.version,
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

fn fail_case_patch(
    case: &Case,
    stage: String,
    context: Value,
    reason: &str,
) -> case_repo::CasePatch {
    case_repo::CasePatch {
        status: CaseStatus::Failed.as_str().into(),
        stage,
        context,
        next_wake_at: None,
        last_error: Some(reason.to_string()),
        run_id: None,
        clear_run_id: false,
        bump_decision: true,
        expected_version: case.version,
    }
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
        &fail_case_patch(case, stage, context, reason),
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

/// LLM / heuristic next action for open/waiting/deciding rows.
/// Do not call on acting — `reconcile_acting_runs` owns those; a wait
/// would drop acting, a launch_agents would overwrite `run_id`.
async fn decide_for_case(state: &AppState, case: &Case) -> CaseDecision {
    debug_assert_ne!(
        CaseStatus::parse(&case.status),
        Some(CaseStatus::Acting),
        "decide_for_case must not run on acting; http_wake_now / claim_due skip it"
    );
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
            let result = proxy::invoke(
                &state.db,
                &state.integrations.credentials_key,
                ProxyRequest {
                    workspace_id: case.workspace_id,
                    provider: provider.clone(),
                    operation: operation.clone(),
                    args,
                    caller: match case.created_by {
                        Some(user_id) => ProxyCaller::Chat { user_id },
                        None => ProxyCaller::System,
                    },
                },
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

/// Acting without a run_id is invisible to reconcile and stays stuck.
fn launch_agents_ok_patch(
    case: &Case,
    stage: String,
    context: Value,
    run_id: Uuid,
) -> case_repo::CasePatch {
    case_repo::CasePatch {
        status: CaseStatus::Acting.as_str().into(),
        stage,
        context,
        next_wake_at: None,
        last_error: None,
        run_id: Some(run_id),
        clear_run_id: false,
        bump_decision: true,
        expected_version: case.version,
    }
}

/// Bad / rejected plan: stay waiting. Never write acting without a run.
fn launch_agents_failed_patch(
    case: &Case,
    stage: String,
    context: Value,
    error: String,
) -> case_repo::CasePatch {
    let next = Utc::now() + chrono::Duration::seconds(LAUNCH_FAIL_RETRY_SECS);
    case_repo::CasePatch {
        status: CaseStatus::Waiting.as_str().into(),
        stage,
        context,
        next_wake_at: Some(next),
        last_error: Some(error),
        run_id: None,
        clear_run_id: false,
        bump_decision: true,
        expected_version: case.version,
    }
}

fn launch_agents_decide_patch(
    case: &Case,
    stage: String,
    context: Value,
    launched: Result<Uuid, String>,
) -> case_repo::CasePatch {
    match launched {
        Ok(run_id) => launch_agents_ok_patch(case, stage, context, run_id),
        Err(error) => launch_agents_failed_patch(case, stage, context, error),
    }
}

fn tasks_from_launch(case: &Case, launch: &Value) -> Result<Vec<PlanTask>, String> {
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
    Ok(tasks)
}

/// Build + validate the fleet plan. A rejected plan never reaches insert.
fn prepare_launch_plan(case: &Case, launch: &Value) -> Result<WorkPlanBody, String> {
    let tasks = tasks_from_launch(case, launch)?;
    let mut plan = build_multi_task_plan(case.workspace_id, None, tasks);
    plan.source = "case".into();
    validate::validate_plan(&plan).map_err(|e| e.to_string())?;
    Ok(plan)
}

async fn apply_launch_agents(
    state: &AppState,
    case: &Case,
    decision: CaseDecision,
    stage: String,
    mut ctx: Value,
) -> AppResult<Case> {
    let launch = decision.launch.clone().unwrap_or_else(|| json!({}));
    let launched = launch_case_plan(state, case, &launch).await;
    match &launched {
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
        }
    }
    case_repo::apply_patch(
        &state.db,
        case.workspace_id,
        case.id,
        &launch_agents_decide_patch(case, stage, ctx, launched),
    )
    .await
    .map_err(|e| AppError::internal(e.to_string()))
}

async fn launch_case_plan(state: &AppState, case: &Case, launch: &Value) -> Result<Uuid, String> {
    let mut plan = prepare_launch_plan(case, launch)?;
    let llm = crate::chat_tools::composer_or_fallback(state, case.workspace_id).await;
    if !llm.provider.is_empty() {
        plan.policy.llm_provider = Some(llm.provider);
    }
    if !llm.model.is_empty() {
        plan.policy.llm_model = Some(llm.model);
    }

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

    const ALL_STATUSES: [CaseStatus; 7] = [
        CaseStatus::Open,
        CaseStatus::Waiting,
        CaseStatus::Deciding,
        CaseStatus::Acting,
        CaseStatus::Completed,
        CaseStatus::Failed,
        CaseStatus::Cancelled,
    ];

    #[test]
    fn transition_matrix_basics() {
        assert!(can_transition(CaseStatus::Open, CaseStatus::Waiting));
        assert!(can_transition(CaseStatus::Waiting, CaseStatus::Deciding));
        assert!(can_transition(CaseStatus::Deciding, CaseStatus::Completed));
        assert!(!can_transition(CaseStatus::Completed, CaseStatus::Open));
        assert!(!can_transition(CaseStatus::Failed, CaseStatus::Waiting));
        assert!(can_transition(CaseStatus::Open, CaseStatus::Open));
    }

    #[test]
    fn acting_can_leave_to_every_non_self_non_impossible() {
        use CaseStatus::*;
        for to in [Open, Waiting, Deciding, Completed, Failed, Cancelled] {
            assert!(
                can_transition(Acting, to),
                "acting → {} must be allowed",
                to.as_str()
            );
        }
        assert!(can_transition(Acting, Acting));
    }

    #[test]
    fn terminals_have_no_outbound_except_same_status() {
        use CaseStatus::*;
        let terminals = [Completed, Failed, Cancelled];
        let all = [
            Open, Waiting, Deciding, Acting, Completed, Failed, Cancelled,
        ];
        for from in terminals {
            for to in all {
                if from == to {
                    assert!(can_transition(from, to), "same-status {from:?}");
                } else {
                    assert!(
                        !can_transition(from, to),
                        "{:?} → {:?} must be forbidden",
                        from,
                        to
                    );
                }
            }
        }
    }

    #[test]
    fn parse_and_terminal_flags() {
        for (wire, want) in [
            ("open", CaseStatus::Open),
            ("waiting", CaseStatus::Waiting),
            ("deciding", CaseStatus::Deciding),
            ("acting", CaseStatus::Acting),
            ("completed", CaseStatus::Completed),
            ("failed", CaseStatus::Failed),
            ("cancelled", CaseStatus::Cancelled),
        ] {
            assert_eq!(CaseStatus::parse(wire), Some(want));
            assert_eq!(want.as_str(), wire);
        }
        assert_eq!(CaseStatus::parse(""), None);
        assert_eq!(CaseStatus::parse("native"), None);
        assert!(CaseStatus::Completed.is_terminal());
        assert!(CaseStatus::Failed.is_terminal());
        assert!(CaseStatus::Cancelled.is_terminal());
        assert!(!CaseStatus::Acting.is_terminal());
        assert!(!CaseStatus::Open.is_terminal());
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
    fn terminal_completed_failed_cancelled_cannot_leave_same_status_ok() {
        for from in [
            CaseStatus::Completed,
            CaseStatus::Failed,
            CaseStatus::Cancelled,
        ] {
            for to in ALL_STATUSES {
                if from == to {
                    assert!(
                        can_transition(from, to),
                        "same-status {from:?} → {to:?} must be allowed"
                    );
                } else {
                    assert!(
                        !can_transition(from, to),
                        "terminal {from:?} → {to:?} must be denied"
                    );
                }
            }
        }
    }

    #[test]
    fn parse_unknown_and_blank_none_all_seven_wires_some() {
        assert_eq!(CaseStatus::parse("unknown"), None);
        assert_eq!(CaseStatus::parse(""), None);
        assert_eq!(CaseStatus::parse("   "), None);
        assert_eq!(CaseStatus::parse("OPEN"), None);

        assert_eq!(CaseStatus::parse("open"), Some(CaseStatus::Open));
        assert_eq!(CaseStatus::parse("waiting"), Some(CaseStatus::Waiting));
        assert_eq!(CaseStatus::parse("deciding"), Some(CaseStatus::Deciding));
        assert_eq!(CaseStatus::parse("acting"), Some(CaseStatus::Acting));
        assert_eq!(CaseStatus::parse("completed"), Some(CaseStatus::Completed));
        assert_eq!(CaseStatus::parse("failed"), Some(CaseStatus::Failed));
        assert_eq!(CaseStatus::parse("cancelled"), Some(CaseStatus::Cancelled));

        for status in ALL_STATUSES {
            assert_eq!(CaseStatus::parse(status.as_str()), Some(status));
        }
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

    /// `force_complete` is a decide-time hint, not a status override. Cancel
    /// keeps the context flag; the wake loop must not complete that row.
    #[test]
    fn heuristic_force_complete_only_when_deciding_does_not_complete_cancelled() {
        let deciding = deciding_case(3, 5);
        assert_eq!(deciding.status, CaseStatus::Deciding.as_str());
        assert_eq!(
            deciding
                .context
                .get("force_complete")
                .and_then(|v| v.as_bool()),
            Some(true),
            "fixture must carry force_complete so cancel leftover is not a no-op"
        );
        assert_eq!(
            heuristic_decision(&deciding).action,
            "complete",
            "force_complete is honored only on the decide path (claimed deciding)"
        );
        assert!(can_transition(CaseStatus::Deciding, CaseStatus::Completed));
        assert!(!CaseStatus::Deciding.is_terminal());

        // POST /cancel clones context, so the flag survives on the cancelled row.
        let cancel = cancel_case_patch(&deciding);
        assert_eq!(cancel.status, CaseStatus::Cancelled.as_str());
        assert_eq!(
            cancel
                .context
                .get("force_complete")
                .and_then(|v| v.as_bool()),
            Some(true),
            "cancel must keep context.force_complete; clearing the flag would hide the hole"
        );
        assert!(
            cancel.next_wake_at.is_none(),
            "cancelled must leave the wake loop so process_due cannot claim it"
        );
        assert!(!case_repo::is_claim_due_status(cancel.status.as_str()));
        assert!(!case_repo::is_claim_due_status(
            CaseStatus::Cancelled.as_str()
        ));

        let cancelled = Case {
            status: cancel.status.clone(),
            context: cancel.context.clone(),
            next_wake_at: cancel.next_wake_at,
            version: deciding.version + 1,
            ..deciding.clone()
        };
        let from = CaseStatus::parse(&cancelled.status).expect("cancelled wire");
        assert_eq!(from, CaseStatus::Cancelled);
        assert!(from.is_terminal());
        assert!(
            !can_transition(from, CaseStatus::Completed),
            "leftover force_complete must not skip FSM: cancelled → completed is illegal"
        );
        assert!(!can_transition(from, CaseStatus::Deciding));
        assert!(!can_transition(from, CaseStatus::Waiting));
        assert!(!can_transition(from, CaseStatus::Open));
        assert!(!can_transition(from, CaseStatus::Acting));

        // Heuristic may still emit complete from the leftover flag. Applying
        // that decision is the hole — FSM / apply_decision / claim_due refuse.
        let hinted = heuristic_decision(&cancelled);
        let would_complete = complete_case_patch(
            &cancelled,
            hinted
                .stage
                .clone()
                .unwrap_or_else(|| cancelled.stage.clone()),
            cancelled.context.clone(),
        );
        assert_eq!(would_complete.status, CaseStatus::Completed.as_str());
        assert!(
            !can_transition(
                from,
                CaseStatus::parse(&would_complete.status).expect("complete status")
            ),
            "complete_case_patch on a cancelled leftover-flag row must not be legal"
        );
        assert_ne!(would_complete.status, CaseStatus::Cancelled.as_str());

        // process_due error unclaim writes waiting — also illegal on cancelled.
        let unclaim = auto_decide_off_unclaim_patch(&cancelled);
        assert!(
            !can_transition(from, CaseStatus::parse(&unclaim.status).expect("unclaim")),
            "wake-loop recover-to-waiting must not revive cancelled after a refused complete"
        );

        // Status-blind heuristic may still emit complete from the leftover
        // flag. Applying it is illegal on every terminal except same-status
        // completed (apply_decision 409s those too).
        for status in ALL_STATUSES {
            let case = Case {
                status: status.as_str().into(),
                context: json!({ "force_complete": true }),
                next_wake_at: if status.is_terminal() {
                    None
                } else {
                    Some(Utc::now())
                },
                ..deciding.clone()
            };
            assert_eq!(
                case.context.get("force_complete").and_then(|v| v.as_bool()),
                Some(true)
            );
            if status.is_terminal() {
                assert!(
                    !case_repo::is_claim_due_status(status.as_str()),
                    "process_due must not claim terminal {status:?} just because the flag is set"
                );
                let patch = complete_case_patch(&case, case.stage.clone(), case.context.clone());
                assert_eq!(patch.status, CaseStatus::Completed.as_str());
                if status != CaseStatus::Completed {
                    assert!(
                        !can_transition(
                            status,
                            CaseStatus::parse(&patch.status).expect("complete")
                        ),
                        "leftover force_complete must not skip FSM: {status:?} → completed"
                    );
                }
            }
        }

        // Live lock: heuristic_force_complete_* in tests/http_api.rs drives
        // process_due against a cancelled leftover-flag row. Helpers here
        // stay extra — do not treat include_str / fn_src as the lock.
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
        assert!(parse_tool_request(&json!({})).is_err());
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
    fn llm_decide_wanted_respects_env_and_providers() {
        // Safety: this test process may have CASE_LLM_DECIDE set; only assert
        // the explicit-off / provider-on combinations via the helper's env branch
        // by checking the boolean logic with known inputs when env is unset.
        let _ = llm_decide_wanted(true, false);
        let _ = llm_decide_wanted(false, false);
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
            assert_eq!(
                process_due_claim_action(case_auto_decide_enabled(Some(raw))),
                ProcessDueClaimAction::UnclaimWaiting,
                "CASE_AUTO_DECIDE={raw:?} must not apply LLM/heuristic decide"
            );
        }
        assert!(case_auto_decide_enabled(None));
        assert!(case_auto_decide_enabled(Some("on")));
        assert!(case_auto_decide_enabled(Some("1")));
        assert_eq!(
            process_due_claim_action(true),
            ProcessDueClaimAction::ApplyDecide
        );
    }

    #[test]
    fn case_auto_decide_off_unclaim_does_not_bump_decision_count() {
        let case = deciding_case(7, 3);
        let patch = auto_decide_off_unclaim_patch(&case);
        assert_eq!(patch.status, CaseStatus::Waiting.as_str());
        assert!(!patch.bump_decision);
        assert_eq!(patch.expected_version, case.version);
        assert_eq!(patch.last_error.as_deref(), Some("CASE_AUTO_DECIDE=off"));
        assert_eq!(patch.next_wake_at, case.next_wake_at);
        // force_complete would complete if decide ran; off-path must stay waiting.
        assert_eq!(heuristic_decision(&case).action, "complete");
        assert_eq!(
            process_due_claim_action(case_auto_decide_enabled(Some("off"))),
            ProcessDueClaimAction::UnclaimWaiting
        );
    }

    #[test]
    fn case_auto_decide_off_failed_unclaim_does_not_leave_deciding() {
        let case = deciding_case(4, 2);
        let (action, retry) = process_due_after_unclaim_failure(&case);
        assert_eq!(action, ProcessDueClaimAction::UnclaimWaiting);
        assert_ne!(action, ProcessDueClaimAction::ApplyDecide);
        assert_eq!(retry.status, CaseStatus::Waiting.as_str());
        assert_ne!(retry.status, CaseStatus::Deciding.as_str());
        assert!(!retry.bump_decision);
        assert_eq!(retry.expected_version, case.version);
        assert_eq!(retry.last_error.as_deref(), Some("CASE_AUTO_DECIDE=off"));
    }

    fn production_src() -> &'static str {
        let src = include_str!("cases.rs");
        match src.find("#[cfg(test)]") {
            Some(i) => &src[..i],
            None => src,
        }
    }

    fn claim_due_repo_src() -> &'static str {
        let src = include_str!("../../fleet-db/src/repos/cases.rs");
        match src.find("#[cfg(test)]") {
            Some(i) => &src[..i],
            None => src,
        }
    }

    fn fn_src<'a>(src: &'a str, sig: &str) -> &'a str {
        let start = src.find(sig).unwrap_or_else(|| panic!("missing {sig}"));
        let after = start + sig.len();
        let end = src[after..]
            .find("\npub async fn ")
            .or_else(|| src[after..].find("\nasync fn "))
            .or_else(|| src[after..].find("\npub fn "))
            .or_else(|| src[after..].find("\nfn "))
            .map(|i| after + i)
            .unwrap_or(src.len());
        &src[start..end]
    }

    /// FSM: a due `next_wake_at` does not make a terminal status claimable
    /// or transitionable into deciding/waiting/open. `claim_due` SET
    /// bypasses `can_transition`, so this is not the product lock — see
    /// `process_due_claim_due_skips_completed_and_cancelled_with_due_wake`.
    #[test]
    fn process_due_must_not_revive_completed_or_cancelled_into_deciding() {
        let due = Some(Utc::now() - chrono::Duration::seconds(30));
        for from in [
            CaseStatus::Completed,
            CaseStatus::Cancelled,
            CaseStatus::Failed,
        ] {
            assert!(
                from.is_terminal(),
                "{from:?} is terminal; process_due must not pick it"
            );
            assert!(
                !can_transition(from, CaseStatus::Deciding),
                "process_due must not move {from:?} to deciding"
            );
            assert!(
                !can_transition(from, CaseStatus::Waiting),
                "process_due unclaim must not revive {from:?} into waiting"
            );
            assert!(
                !can_transition(from, CaseStatus::Open),
                "process_due must not reopen {from:?}"
            );
            let case = Case {
                status: from.as_str().into(),
                next_wake_at: due,
                ..deciding_case(0, 1)
            };
            let parsed = CaseStatus::parse(&case.status).expect("wire status");
            assert!(
                parsed.is_terminal(),
                "due next_wake_at must not change terminal status {from:?}"
            );
            assert!(!can_transition(parsed, CaseStatus::Deciding));
            // Unclaim / decide patches would write waiting — FSM must refuse.
            let unclaim = auto_decide_off_unclaim_patch(&case);
            let unclaim_to = CaseStatus::parse(&unclaim.status).expect("unclaim status");
            assert!(
                !can_transition(parsed, unclaim_to),
                "process_due unclaim must not apply {from:?} → {unclaim_to:?}"
            );
        }
        assert!(can_transition(CaseStatus::Open, CaseStatus::Deciding));
        assert!(can_transition(CaseStatus::Waiting, CaseStatus::Deciding));
    }

    #[test]
    fn process_due_claim_sources_exclude_acting_and_already_deciding() {
        assert!(
            !case_repo::is_claim_due_status(CaseStatus::Acting.as_str()),
            "acting is owned by reconcile_acting_runs, not claim_due"
        );
        assert!(
            !case_repo::is_claim_due_status(CaseStatus::Deciding.as_str()),
            "already-claimed deciding rows must not be claimed again"
        );
        for status in ALL_STATUSES {
            let claimable = case_repo::is_claim_due_status(status.as_str());
            assert_eq!(
                claimable,
                matches!(status, CaseStatus::Open | CaseStatus::Waiting),
                "{status:?} claimable={claimable}"
            );
            if status.is_terminal() {
                assert!(!claimable);
            }
        }
    }

    /// `process_due` must not have its own status-blind SELECT. Revival is
    /// prevented only if `claim_due` stays open/waiting. Do not forbid
    /// mentioning completed/cancelled — a real skip-guard would.
    #[test]
    fn process_due_loop_only_claims_via_claim_due() {
        let wake = fn_src(production_src(), "pub async fn process_due");
        assert!(
            wake.contains("case_repo::claim_due"),
            "process_due must claim through claim_due:\n{wake}"
        );
        assert!(
            wake.contains("reclaim_stale_deciding"),
            "process_due must reclaim stale deciding before claiming"
        );
        assert!(
            !wake.contains("FROM cases"),
            "process_due must not issue a raw cases SELECT that could pick terminals:\n{wake}"
        );
    }

    /// Pin the repo SQL `process_due` actually runs. Status and due-wake
    /// must be AND-ed: `status IN (...) OR next_wake_at <= now()` would
    /// revive a due completed row. Not the product lock — see cases_repos.
    #[test]
    fn process_due_claim_due_sql_excludes_terminal_statuses() {
        let claim = fn_src(claim_due_repo_src(), "pub async fn claim_due");
        assert!(
            claim.contains("WHERE status IN ('open', 'waiting')"),
            "claim_due must only SELECT open/waiting:\n{claim}"
        );
        assert!(
            claim.contains("AND next_wake_at IS NOT NULL"),
            "due-wake must be AND-ed with the status filter, not OR:\n{claim}"
        );
        assert!(
            claim.contains("AND next_wake_at <= now()"),
            "due-wake must be AND-ed with the status filter, not OR:\n{claim}"
        );
        assert!(
            !claim.contains("OR next_wake_at"),
            "OR-ing next_wake_at with the status IN-list would revive a due terminal:\n{claim}"
        );
        assert!(
            claim.contains("SET status = 'deciding'"),
            "claim_due is the only path process_due uses to enter deciding"
        );

        let reclaim = fn_src(claim_due_repo_src(), "pub async fn reclaim_stale_deciding");
        assert!(
            reclaim.contains("WHERE status = 'deciding'"),
            "reclaim must not scan completed/cancelled"
        );
    }

    /// A case that already used the 200-decision budget must fail closed on
    /// the next decide/wake. Removing the check, flipping `>=` to `>`, or
    /// leaving the row deciding/acting would make this fail.
    #[test]
    fn max_decisions_at_cap_fails_closed() {
        assert_eq!(MAX_DECISIONS_PER_CASE, 200);

        for n in [0, 1, 199] {
            assert!(
                max_decisions_fail_reason(n).is_none(),
                "decision_count={n} is under the cap and must still decide"
            );
        }
        for n in [200, 201, 1_000, i32::MAX] {
            let reason = max_decisions_fail_reason(n).unwrap_or_else(|| {
                panic!("decision_count={n} must fail closed (cap missing or skipped)")
            });
            assert_eq!(reason, "max decisions exceeded");

            let case = deciding_case(n, 4);
            let patch = fail_case_patch(&case, case.stage.clone(), case.context.clone(), reason);
            assert_eq!(patch.status, CaseStatus::Failed.as_str());
            assert_ne!(patch.status, CaseStatus::Deciding.as_str());
            assert_ne!(patch.status, CaseStatus::Acting.as_str());
            assert_ne!(patch.status, CaseStatus::Waiting.as_str());
            assert!(
                patch.next_wake_at.is_none(),
                "failed-at-cap case must leave the wake loop (no overflow)"
            );
            assert_eq!(patch.last_error.as_deref(), Some("max decisions exceeded"));
            assert_eq!(patch.expected_version, case.version);
            assert!(can_transition(CaseStatus::Deciding, CaseStatus::Failed));
            assert!(can_transition(CaseStatus::Acting, CaseStatus::Failed));
            assert!(can_transition(CaseStatus::Waiting, CaseStatus::Failed));
        }

        let apply = fn_src(production_src(), "pub async fn apply_decision");
        assert!(
            apply.contains("max_decisions_fail_reason(case.decision_count)"),
            "apply_decision must consult the cap helper; a skipped check would keep deciding:\n{apply}"
        );
        let cap_at = apply
            .find("max_decisions_fail_reason")
            .expect("apply_decision must call max_decisions_fail_reason");
        let fail_at = apply
            .find("fail_case")
            .expect("cap exceeded must call fail_case, not wait/act");
        let dispatch_at = apply
            .find("match action")
            .expect("apply_decision must dispatch actions after the cap");
        assert!(
            cap_at < fail_at && fail_at < dispatch_at,
            "cap must fail_case before wait/complete/launch_agents:\n{apply}"
        );
        assert!(
            !apply[cap_at..dispatch_at].contains("Waiting")
                && !apply[cap_at..dispatch_at].contains("Acting"),
            "cap branch must not leave the case waiting or acting:\n{apply}"
        );

        let helper = fn_src(production_src(), "fn max_decisions_fail_reason");
        assert!(
            helper.contains("decision_count >= MAX_DECISIONS_PER_CASE"),
            "cap is inclusive (200 already fails); `>` would skip the 200th:\n{helper}"
        );

        let fail_src = fn_src(production_src(), "fn fail_case_patch");
        assert!(
            fail_src.contains("CaseStatus::Failed"),
            "fail_case_patch must write failed:\n{fail_src}"
        );
        assert!(
            fail_src.contains("next_wake_at: None"),
            "fail_case_patch must clear next_wake_at:\n{fail_src}"
        );

        let fail_with = fn_src(production_src(), "async fn fail_case_with");
        assert!(
            fail_with.contains("fail_case_patch"),
            "fail_case_with must apply fail_case_patch (Failed, no wake)"
        );

        for (sig, label) in [
            ("async fn http_wake_now", "http wake"),
            ("async fn http_decide_manual", "manual decide"),
            ("pub async fn process_due", "process_due wake"),
        ] {
            let src = fn_src(production_src(), sig);
            assert!(
                src.contains("apply_decision"),
                "{label} must go through apply_decision so the cap is not skipped:\n{src}"
            );
        }
    }

    fn acting_case_with_run(run_id: Uuid, version: i64) -> Case {
        Case {
            id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            kind: "generic".into(),
            title: "acting-cancel".into(),
            policy_md: "p".into(),
            status: CaseStatus::Acting.as_str().into(),
            stage: "fleet".into(),
            context: json!({ "fleet_run": { "id": run_id, "status": "accepted" } }),
            next_wake_at: Some(Utc::now() - chrono::Duration::seconds(30)),
            last_error: None,
            run_id: Some(run_id),
            decision_count: 3,
            created_by: None,
            version,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// `apply_patch` writes `COALESCE($9, run_id)` unless `clear_run_id`.
    /// Observe must leave the acting fleet run linked.
    fn patch_keeps_run_id(existing: Option<Uuid>, patch: &case_repo::CasePatch) -> Option<Uuid> {
        if patch.clear_run_id {
            None
        } else {
            patch.run_id.or(existing)
        }
    }

    /// Observe on acting must keep `status=acting` and the fleet `run_id`.
    /// HTTP / chat default `wake=true`; that must not reopen the case or
    /// bump `next_wake_at`. Resetting to open would drop the row from
    /// `list_acting_with_run` and let `claim_due` pick it.
    #[test]
    fn observe_on_acting_keeps_status_acting_and_does_not_clobber_run_id() {
        let run_id = Uuid::now_v7();
        let acting = acting_case_with_run(run_id, 8);
        assert_eq!(acting.status, CaseStatus::Acting.as_str());
        assert_eq!(acting.run_id, Some(run_id));
        let leftover_wake = acting.next_wake_at;
        assert!(leftover_wake.is_some());
        assert!(!CaseStatus::Acting.is_terminal());
        assert!(
            !case_repo::is_claim_due_status(CaseStatus::Acting.as_str()),
            "acting is owned by reconcile_acting_runs, not claim_due"
        );
        assert!(
            case_repo::is_claim_due_status(CaseStatus::Open.as_str()),
            "reset-to-open would hand the fleet-linked row to claim_due"
        );

        // HTTP default is wake=true (`unwrap_or(true)`). Must still keep acting.
        for wake in [true, false] {
            let patch = observe_case_patch(&acting, "human fact while agents run", wake);
            assert_eq!(
                patch.status,
                CaseStatus::Acting.as_str(),
                "observe wake={wake} on acting must keep acting, not reopen"
            );
            assert_ne!(
                patch.status,
                CaseStatus::Open.as_str(),
                "observe wake={wake} must not reset acting → open (drops the fleet run link)"
            );
            assert_ne!(patch.status, CaseStatus::Waiting.as_str());
            assert_ne!(patch.status, CaseStatus::Deciding.as_str());
            assert_ne!(patch.status, CaseStatus::Cancelled.as_str());
            assert!(
                !case_repo::is_claim_due_status(patch.status.as_str()),
                "observe wake={wake} must not make an acting case claim_due"
            );
            assert!(
                !patch.clear_run_id,
                "observe must not NULL run_id; apply_patch COALESCE keeps the acting run when clear_run_id is false and run_id is None"
            );
            assert!(
                patch.run_id.is_none() || patch.run_id == Some(run_id),
                "observe must not replace the fleet run with a different id"
            );
            assert_eq!(
                patch_keeps_run_id(acting.run_id, &patch),
                Some(run_id),
                "observe wake={wake} must keep the fleet run_id"
            );
            assert_eq!(
                patch.next_wake_at, leftover_wake,
                "observe must not wake acting (even wake=true); next_wake_at stays put"
            );
            assert!(
                !patch.bump_decision,
                "observe is not a decide; bumping would look like another engine step"
            );
            assert_eq!(patch.expected_version, acting.version);
            assert_eq!(patch.stage, acting.stage);
            assert_eq!(
                patch.context["fleet_run"]["id"],
                json!(run_id),
                "observe must keep context.fleet_run"
            );
            let notes = patch.context["observations"]
                .as_array()
                .expect("observe must append context.observations");
            assert!(
                notes
                    .iter()
                    .any(|n| n["note"].as_str() == Some("human fact while agents run")),
                "observe must record the note: {}",
                patch.context
            );
        }

        // Control: observe on waiting *does* reopen + wake so the acting
        // keep is not "observe never changes status".
        let waiting = Case {
            status: CaseStatus::Waiting.as_str().into(),
            run_id: None,
            next_wake_at: Some(Utc::now() + chrono::Duration::hours(1)),
            context: json!({}),
            ..acting.clone()
        };
        let woke = observe_case_patch(&waiting, "nudge waiting", true);
        assert_eq!(
            woke.status,
            CaseStatus::Open.as_str(),
            "observe wake=true on waiting still reopens; acting is the exception"
        );
        assert!(case_repo::is_claim_due_status(woke.status.as_str()));
        let woke_at = woke
            .next_wake_at
            .expect("wake=true on waiting bumps next_wake_at");
        assert!(
            woke_at < waiting.next_wake_at.unwrap(),
            "wake=true on waiting must schedule now, not keep the far-future wake"
        );
        assert_eq!(patch_keeps_run_id(waiting.run_id, &woke), None);

        // A reset-to-open patch on the acting row would drop the fleet link
        // even if COALESCE kept run_id: list_acting_with_run needs both.
        let reopen = case_repo::CasePatch {
            status: CaseStatus::Open.as_str().into(),
            ..observe_case_patch(&acting, "would drop the run link", true)
        };
        assert_eq!(reopen.status, CaseStatus::Open.as_str());
        assert!(case_repo::is_claim_due_status(reopen.status.as_str()));
        assert_eq!(
            patch_keeps_run_id(acting.run_id, &reopen),
            Some(run_id),
            "COALESCE can keep run_id after a status reset — status is the drop"
        );
        assert_ne!(
            reopen.status,
            CaseStatus::Acting.as_str(),
            "open + leftover run_id is invisible to list_acting_with_run"
        );

        // Live POST /observe on acting (default wake=true keeps acting + run_id;
        // wake=false same; waiting+wake=true still reopens) is locked in
        // crates/fleet-server/tests/http_api.rs (`observe_on_acting`).
        // These observe_case_patch field asserts stay; do not treat
        // fn_src/include_str as the only observe-on-acting lock.
    }

    /// POST /cancel on acting must be terminal and drop `run_id`.
    /// `apply_patch` is `COALESCE` unless `clear_run_id` — leaving the fleet
    /// run linked lets `reconcile_acting_runs` move the row back to waiting
    /// when that run later completes, and `process_due` would then decide
    /// (and can relaunch). Do not weaken `clear_run_id` to match a keep-run
    /// patch.
    #[test]
    fn cancel_acting_clears_run_and_does_not_relaunch_when_fleet_run_completes() {
        let run_id = Uuid::now_v7();
        let acting = acting_case_with_run(run_id, 8);
        assert_eq!(acting.status, CaseStatus::Acting.as_str());
        assert_eq!(acting.run_id, Some(run_id));
        assert!(acting.next_wake_at.is_some());
        assert!(can_transition(CaseStatus::Acting, CaseStatus::Cancelled));
        assert!(!CaseStatus::Acting.is_terminal());

        let patch = cancel_case_patch(&acting);
        assert_eq!(
            patch.status,
            CaseStatus::Cancelled.as_str(),
            "POST cancel on acting must mark cancelled, not waiting/acting"
        );
        assert_ne!(patch.status, CaseStatus::Waiting.as_str());
        assert_ne!(patch.status, CaseStatus::Acting.as_str());
        assert_ne!(patch.status, CaseStatus::Deciding.as_str());
        assert_ne!(patch.status, CaseStatus::Open.as_str());
        assert!(
            patch.clear_run_id,
            "cancel must NULL run_id; apply_patch COALESCE keeps the acting run when clear_run_id is false and run_id is None"
        );
        assert!(
            patch.run_id.is_none(),
            "cancel must not write a new run_id (that would relaunch)"
        );
        assert!(
            patch.next_wake_at.is_none(),
            "cancelled case must leave the wake loop so process_due cannot claim it"
        );
        assert!(
            !patch.bump_decision,
            "cancel is not a decide; bumping would look like another engine step"
        );
        assert_eq!(patch.expected_version, acting.version);
        assert_eq!(patch.stage, acting.stage);
        assert_eq!(patch.context, acting.context);

        let cancelled_status = CaseStatus::parse(&patch.status).expect("cancel status");
        assert!(
            cancelled_status.is_terminal(),
            "POST cancel on acting must be terminal"
        );
        assert!(!can_transition(cancelled_status, CaseStatus::Waiting));
        assert!(!can_transition(cancelled_status, CaseStatus::Acting));
        assert!(!can_transition(cancelled_status, CaseStatus::Deciding));
        assert!(!can_transition(cancelled_status, CaseStatus::Open));
        assert!(!case_repo::is_claim_due_status(patch.status.as_str()));
        assert!(!case_repo::is_claim_due_status(
            CaseStatus::Cancelled.as_str()
        ));
        assert!(!case_repo::is_claim_due_status(CaseStatus::Acting.as_str()));

        // After apply_patch with clear_run_id, the row is cancelled + no run.
        // list_acting_with_run requires both status=acting AND run_id IS NOT NULL.
        assert_ne!(patch.status, CaseStatus::Acting.as_str());
        assert!(patch.clear_run_id && patch.run_id.is_none());

        // Fleet run later completes: reconcile writes Waiting. FSM must refuse
        // that patch on the cancelled row (SQL apply_patch does not check FSM).
        let revive = case_repo::CasePatch {
            status: CaseStatus::Waiting.as_str().into(),
            stage: acting.stage.clone(),
            context: acting.context.clone(),
            next_wake_at: Some(Utc::now()),
            last_error: None,
            run_id: None,
            clear_run_id: false,
            bump_decision: false,
            expected_version: acting.version + 1,
        };
        let revive_to = CaseStatus::parse(&revive.status).expect("reconcile status");
        assert!(
            !can_transition(cancelled_status, revive_to),
            "reconcile_acting_runs must not revive cancelled → {revive_to:?} when the fleet run completes"
        );
        assert!(
            !can_transition(cancelled_status, CaseStatus::Acting),
            "process_due / reconcile must not relaunch into acting"
        );

        // Live POST /cancel on acting then process_due after the fleet run
        // completes is locked in crates/fleet-server/tests/http_api.rs
        // (`cancel_acting_clears_run`). These cancel_case_patch field asserts
        // stay; do not treat include_str/fn_src as the only lock.
    }

    fn launch_deciding_case() -> Case {
        Case {
            id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            kind: "generic".into(),
            title: "ship dark mode".into(),
            policy_md: "implement dark mode".into(),
            status: CaseStatus::Deciding.as_str().into(),
            stage: "start".into(),
            context: json!({}),
            next_wake_at: Some(Utc::now()),
            last_error: None,
            run_id: None,
            decision_count: 2,
            created_by: None,
            version: 5,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// `launch_agents` writes `acting` only together with the fleet `run_id`.
    /// A rejected plan (cycle / dup id / unknown dep / insert fail) must stay
    /// waiting — acting with no run is invisible to `list_acting_with_run`
    /// and skipped by `claim_due`.
    #[test]
    fn launch_agents_decide_sets_acting_with_run_id_and_bad_plan_does_not_act_without_run() {
        let case = launch_deciding_case();
        assert_eq!(case.status, CaseStatus::Deciding.as_str());
        assert!(case.run_id.is_none());
        assert!(can_transition(CaseStatus::Deciding, CaseStatus::Acting));
        assert!(can_transition(CaseStatus::Deciding, CaseStatus::Waiting));
        assert!(!case_repo::is_claim_due_status(CaseStatus::Acting.as_str()));

        let parsed = parse_decision_json(
            r#"{"action":"launch_agents","launch":{"title":"dark","spec":"do it"}}"#,
        )
        .expect("launch_agents decision");
        assert_eq!(parsed.action, "launch_agents");
        assert!(parsed.launch.is_some());

        let good = json!({ "title": "dark", "spec": "do it" });
        let plan = prepare_launch_plan(&case, &good).expect("valid launch must prepare");
        assert_eq!(plan.source, "case");
        assert_eq!(plan.project_id, case.workspace_id);
        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(plan.tasks[0].plan_task_id, "t1");
        assert_eq!(plan.tasks[0].title, "dark");

        let run_id = Uuid::now_v7();
        let ok =
            launch_agents_decide_patch(&case, case.stage.clone(), case.context.clone(), Ok(run_id));
        assert_eq!(
            ok.status,
            CaseStatus::Acting.as_str(),
            "successful launch_agents must set acting"
        );
        assert_eq!(
            ok.run_id,
            Some(run_id),
            "acting must carry the inserted fleet run_id"
        );
        assert!(
            !ok.clear_run_id,
            "success must not NULL the run we just linked"
        );
        assert!(
            ok.next_wake_at.is_none(),
            "acting is owned by reconcile_acting_runs, not claim_due"
        );
        assert!(ok.last_error.is_none());
        assert!(ok.bump_decision);
        assert_eq!(ok.expected_version, case.version);
        assert_ne!(ok.status, CaseStatus::Waiting.as_str());
        assert_ne!(ok.status, CaseStatus::Deciding.as_str());
        assert!(ok.run_id.is_some());

        let cycle = json!({
            "tasks": [
                {"id": "a", "title": "A", "depends_on": ["b"]},
                {"id": "b", "title": "B", "depends_on": ["a"]}
            ]
        });
        let dup = json!({
            "tasks": [
                {"id": "t1", "title": "A"},
                {"id": "t1", "title": "B"}
            ]
        });
        let missing = json!({
            "tasks": [{"id": "t1", "title": "A", "depends_on": ["missing"]}]
        });
        for (launch, needle) in [
            (&cycle, "cycle"),
            (&dup, "duplicate"),
            (&missing, "depends_on"),
        ] {
            let err = prepare_launch_plan(&case, launch).expect_err(&format!(
                "bad launch {launch} must fail validate before insert"
            ));
            assert!(
                err.to_lowercase().contains(needle),
                "expected {needle:?} in {err}"
            );
            let failed = launch_agents_decide_patch(
                &case,
                case.stage.clone(),
                case.context.clone(),
                Err(err.clone()),
            );
            assert_eq!(
                failed.status,
                CaseStatus::Waiting.as_str(),
                "bad plan must wait, not act: {err}"
            );
            assert_ne!(
                failed.status,
                CaseStatus::Acting.as_str(),
                "a bad plan must not leave the case acting with no run: {err}"
            );
            assert!(
                failed.run_id.is_none(),
                "failed launch must not invent a run_id: {err}"
            );
            assert!(
                !failed.clear_run_id,
                "failed launch does not clear a run (there is none)"
            );
            assert_eq!(failed.last_error.as_deref(), Some(err.as_str()));
            let wake = failed
                .next_wake_at
                .expect("failed launch must re-enter the wake loop");
            let delta = (wake - Utc::now()).num_seconds();
            assert!(
                (LAUNCH_FAIL_RETRY_SECS - 5..=LAUNCH_FAIL_RETRY_SECS).contains(&delta),
                "failed launch retry must be ~{LAUNCH_FAIL_RETRY_SECS}s, got {delta}s"
            );
            assert!(failed.bump_decision);
            assert_eq!(failed.expected_version, case.version);
        }

        for err in [
            "launch_agents: no tasks",
            "insert_accepted_plan failed",
            "ensure_project failed",
            "",
        ] {
            let failed = launch_agents_decide_patch(
                &case,
                case.stage.clone(),
                case.context.clone(),
                Err(err.into()),
            );
            assert_ne!(
                failed.status,
                CaseStatus::Acting.as_str(),
                "any launch Err, including {err:?}, must not write acting"
            );
            assert_eq!(failed.status, CaseStatus::Waiting.as_str());
            assert!(failed.run_id.is_none());
        }

        // Live POST /decide launch_agents (acting + uuid run_id vs waiting +
        // no run / no new fleet run) is locked in
        // crates/fleet-server/tests/http_api.rs
        // (`launch_agents_decide_*`). These helper field asserts stay; do not
        // treat include_str/fn_src as the only acting+run_id lock.
    }

    fn terminal_if_block(fn_body: &str) -> &str {
        let start = fn_body
            .find("if from.is_terminal()")
            .or_else(|| fn_body.find("if status.is_terminal()"))
            .unwrap_or_else(|| panic!("missing is_terminal gate:\n{fn_body}"));
        let rest = &fn_body[start..];
        let end = rest
            .find('}')
            .unwrap_or_else(|| panic!("unclosed terminal gate:\n{rest}"));
        &rest[..=end]
    }

    /// POST /decide on completed/cancelled/failed is 409 Conflict.
    /// Must fail closed: do not reopen and do not append a decided event.
    /// 400 would treat a legal body as invalid and invite a retry. Do not
    /// weaken this to BAD_REQUEST to match today's `AppError::bad_request`.
    #[test]
    fn decide_on_terminal_is_conflict_409_and_does_not_reopen_or_append() {
        use axum::http::StatusCode;
        use axum::response::IntoResponse;

        let conflict = AppError::conflict("case is terminal");
        let resp = conflict.into_response();
        assert_eq!(
            resp.status(),
            StatusCode::CONFLICT,
            "AppError::conflict is the 409 wire status for POST /decide on a terminal case"
        );
        assert_ne!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "400 is the wrong class: a well-formed decide on a finished case is a conflict"
        );

        for from in [
            CaseStatus::Completed,
            CaseStatus::Cancelled,
            CaseStatus::Failed,
        ] {
            assert!(from.is_terminal(), "{from:?} is a terminal decide target");
            for to in [
                CaseStatus::Open,
                CaseStatus::Waiting,
                CaseStatus::Deciding,
                CaseStatus::Acting,
            ] {
                assert!(
                    !can_transition(from, to),
                    "decide must not reopen {from:?} → {to:?}"
                );
            }
            // Manual decide first claims to deciding, then apply_decision
            // writes waiting/completed/failed and bumps decision_count.
            let case = Case {
                status: from.as_str().into(),
                decision_count: 4,
                version: 3,
                ..deciding_case(4, 3)
            };
            let claim = case_repo::CasePatch {
                status: CaseStatus::Deciding.as_str().into(),
                stage: case.stage.clone(),
                context: case.context.clone(),
                next_wake_at: case.next_wake_at,
                last_error: None,
                run_id: None,
                clear_run_id: false,
                bump_decision: false,
                expected_version: case.version,
            };
            let decide = case_repo::CasePatch {
                status: CaseStatus::Waiting.as_str().into(),
                stage: case.stage.clone(),
                context: case.context.clone(),
                next_wake_at: Some(Utc::now()),
                last_error: None,
                run_id: None,
                clear_run_id: false,
                bump_decision: true,
                expected_version: case.version,
            };
            assert!(
                !can_transition(from, CaseStatus::parse(&claim.status).unwrap()),
                "claim-to-deciding must not apply on {from:?}"
            );
            assert!(
                !can_transition(from, CaseStatus::parse(&decide.status).unwrap()),
                "decide wait patch must not apply on {from:?}"
            );
            assert!(
                decide.bump_decision,
                "a successful decide would bump decision_count; terminal refuse must not"
            );
        }

        // Control: a still-open case is allowed to enter deciding / apply a wait.
        assert!(can_transition(CaseStatus::Open, CaseStatus::Deciding));
        assert!(can_transition(CaseStatus::Waiting, CaseStatus::Deciding));
        assert!(can_transition(CaseStatus::Deciding, CaseStatus::Waiting));
        assert!(!CaseStatus::Open.is_terminal());
        assert!(!CaseStatus::Acting.is_terminal());

        let manual = fn_src(production_src(), "async fn http_decide_manual");
        let manual_gate = terminal_if_block(manual);
        assert!(
            manual_gate.contains("return Err(AppError::conflict"),
            "POST /decide on terminal must 409 via AppError::conflict, not 400:\n{manual_gate}"
        );
        assert!(
            !manual_gate.contains("AppError::bad_request"),
            "400 bad_request would invite a client retry of a finished case:\n{manual_gate}"
        );
        assert!(
            !manual_gate.contains("apply_decision")
                && !manual_gate.contains("apply_patch")
                && !manual_gate.contains("append_event"),
            "terminal refuse must return before claim, decide, or a decided event:\n{manual_gate}"
        );
        let apply_at = manual
            .find("apply_decision")
            .expect("non-terminal POST /decide still applies a decision");
        let term_at = manual
            .find("is_terminal")
            .expect("POST /decide must gate on is_terminal");
        assert!(
            term_at < apply_at,
            "terminal 409 must run before apply_decision:\n{manual}"
        );

        let apply = fn_src(production_src(), "pub async fn apply_decision");
        let apply_gate = terminal_if_block(apply);
        assert!(
            apply_gate.contains("return Err(AppError::conflict"),
            "apply_decision on terminal must 409, not 400, and must not fall through:\n{apply_gate}"
        );
        assert!(
            !apply_gate.contains("AppError::bad_request"),
            "apply_decision terminal refuse must not be 400:\n{apply_gate}"
        );
        assert!(
            !apply_gate.contains("append_event") && !apply_gate.contains("apply_patch"),
            "apply_decision must not append decided or patch a terminal row:\n{apply_gate}"
        );
        let decided_at = apply
            .find("\"decided\"")
            .expect("apply_decision appends a decided event only after the gate");
        let apply_term_at = apply
            .find("is_terminal")
            .expect("apply_decision must gate on is_terminal");
        assert!(
            apply_term_at < decided_at,
            "must not append decided before the terminal 409:\n{apply}"
        );
    }

    /// POST /decide action=complete on an acting case is allowed (UI Complete
    /// while a fleet run is in flight). Acting is not terminal — do not 409
    /// or 400 it. The handler claims to deciding, then `apply_decision`
    /// writes completed with `next_wake_at: None`. A leftover due wake must
    /// not survive; fail/cancel already clear it. Do not refuse acting, and
    /// do not keep the wake.
    #[test]
    fn manual_complete_from_acting_is_allowed_and_clears_wake() {
        use axum::http::StatusCode;
        use axum::response::IntoResponse;

        let acting = acting_case_with_run(Uuid::now_v7(), 8);
        assert_eq!(acting.status, CaseStatus::Acting.as_str());
        assert!(
            acting.next_wake_at.is_some(),
            "fixture must start with a leftover wake so complete is not a no-op clear"
        );
        assert!(
            !CaseStatus::Acting.is_terminal(),
            "acting must not take the POST /decide terminal 409"
        );
        assert!(can_transition(CaseStatus::Acting, CaseStatus::Deciding));
        assert!(can_transition(CaseStatus::Acting, CaseStatus::Completed));
        assert!(can_transition(CaseStatus::Deciding, CaseStatus::Completed));

        // UI Complete: POST /decide { decision: { action: "complete", note } }.
        let parsed = parse_decision_json(
            r#"{"action":"complete","note":"completed from UI","reason":"done"}"#,
        )
        .expect("UI Complete body");
        assert_eq!(parsed.action, "complete");

        // HTTP decide first claims non-deciding rows to deciding (no bump),
        // preserving next_wake_at. Acting must be allowed through this step.
        let claim = case_repo::CasePatch {
            status: CaseStatus::Deciding.as_str().into(),
            stage: acting.stage.clone(),
            context: acting.context.clone(),
            next_wake_at: acting.next_wake_at,
            last_error: None,
            run_id: None,
            clear_run_id: false,
            bump_decision: false,
            expected_version: acting.version,
        };
        assert_eq!(claim.status, CaseStatus::Deciding.as_str());
        assert!(
            can_transition(
                CaseStatus::Acting,
                CaseStatus::parse(&claim.status).expect("claim status")
            ),
            "claim-to-deciding on acting must be a legal decide step"
        );
        assert_eq!(
            claim.next_wake_at, acting.next_wake_at,
            "claim preserves wake; complete owns the clear"
        );
        assert!(!claim.bump_decision);

        // After the claim apply_patch, version is bumped; apply_decision
        // complete runs against the deciding row (same leftover wake).
        let claimed = Case {
            status: CaseStatus::Deciding.as_str().into(),
            version: acting.version + 1,
            next_wake_at: acting.next_wake_at,
            last_error: None,
            ..acting.clone()
        };
        assert_eq!(claimed.status, CaseStatus::Deciding.as_str());
        assert_eq!(claimed.id, acting.id);
        assert!(claimed.next_wake_at.is_some());

        let patch = complete_case_patch(&claimed, claimed.stage.clone(), claimed.context.clone());
        assert_eq!(
            patch.status,
            CaseStatus::Completed.as_str(),
            "manual complete from acting must mark completed, not waiting/acting"
        );
        assert_ne!(patch.status, CaseStatus::Acting.as_str());
        assert_ne!(patch.status, CaseStatus::Deciding.as_str());
        assert_ne!(patch.status, CaseStatus::Waiting.as_str());
        assert_ne!(patch.status, CaseStatus::Open.as_str());
        assert!(
            patch.next_wake_at.is_none(),
            "complete must clear next_wake_at so a leftover due wake cannot stay scheduled"
        );
        assert!(
            patch.bump_decision,
            "complete is a decide; it must bump decision_count"
        );
        assert_eq!(patch.expected_version, claimed.version);
        assert!(patch.last_error.is_none());

        let done = CaseStatus::parse(&patch.status).expect("complete status");
        assert!(
            done.is_terminal(),
            "manual complete from acting must be terminal"
        );
        assert!(!can_transition(done, CaseStatus::Waiting));
        assert!(!can_transition(done, CaseStatus::Acting));
        assert!(!can_transition(done, CaseStatus::Deciding));
        assert!(!can_transition(done, CaseStatus::Open));
        assert!(!case_repo::is_claim_due_status(patch.status.as_str()));
        assert!(!case_repo::is_claim_due_status(
            CaseStatus::Completed.as_str()
        ));
        assert!(!case_repo::is_claim_due_status(CaseStatus::Acting.as_str()));

        // Direct complete from acting (apply_decision without the claim) is
        // also allowed — do not add an acting skip inside the complete arm.
        let from_acting =
            complete_case_patch(&acting, acting.stage.clone(), acting.context.clone());
        assert_eq!(from_acting.status, CaseStatus::Completed.as_str());
        assert!(from_acting.next_wake_at.is_none());
        assert_eq!(from_acting.expected_version, acting.version);

        // 409 is only for terminals. Acting must not share that refuse.
        let conflict = AppError::conflict("case is terminal").into_response();
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        assert_ne!(
            conflict.status(),
            StatusCode::BAD_REQUEST,
            "a well-formed complete on acting is not a 400"
        );

        // Live POST /decide action=complete from acting (200 completed,
        // leftover wake cleared, decided+completed events) is locked in
        // crates/fleet-server/tests/http_api.rs (`complete_from_acting`).
        // These complete_case_patch field asserts stay; do not treat
        // include_str/fn_src as the only complete-from-acting lock.
    }

    static WAKE_INTERVAL_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Serialize `CASE_WAKE_INTERVAL_S` mutation and restore even if `f` panics.
    /// Drop the mutex before `resume_unwind` so a failing lock does not poison siblings.
    fn with_wake_interval_env<T>(f: impl FnOnce() -> T) -> T {
        let g = WAKE_INTERVAL_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var("CASE_WAKE_INTERVAL_S").ok();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        unsafe {
            match previous {
                Some(v) => std::env::set_var("CASE_WAKE_INTERVAL_S", v),
                None => std::env::remove_var("CASE_WAKE_INTERVAL_S"),
            }
        }
        drop(g);
        match result {
            Ok(v) => v,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    /// `main.rs` is `loop { sleep(wake_interval()); process_due }`.
    /// `CASE_WAKE_INTERVAL_S=0` must not become `Duration::ZERO` (busy-loop).
    #[test]
    fn wake_interval_zero_or_huge_must_not_spin() {
        with_wake_interval_env(|| {
            // 0 / sub-5 / whitespace-0 / negative must not busy-loop process_due.
            // Product floors at 5s; parse-fail falls through to the 15s default.
            // A wrap of -1 to u64::MAX would also pass a naive `> ZERO` check.
            for raw in ["0", "0 ", " 0", "00", "1", "4", "-1", "-5", " -1 "] {
                unsafe { std::env::set_var("CASE_WAKE_INTERVAL_S", raw) };
                let interval = std::panic::catch_unwind(wake_interval).unwrap_or_else(|_| {
                    panic!("CASE_WAKE_INTERVAL_S={raw:?} must not panic Duration::from_secs")
                });
                assert!(
                    interval > Duration::ZERO,
                    "CASE_WAKE_INTERVAL_S={raw:?} must not yield {interval:?} \
                     (tokio::time::sleep(ZERO) busy-loops process_due)"
                );
                assert!(
                    interval >= Duration::from_secs(5),
                    "CASE_WAKE_INTERVAL_S={raw:?} must floor at 5s (got {interval:?}); \
                     sub-5 including ZERO would spin or hammer claim_due"
                );
                assert!(
                    interval <= Duration::from_secs(CASE_WAKE_INTERVAL_S),
                    "CASE_WAKE_INTERVAL_S={raw:?} must clamp to the 5s floor or the 15s \
                     default, not wrap to a huge Duration ({interval:?})"
                );
            }

            // Huge but legal (one day). Must not panic Duration::from_secs or wrap to ZERO.
            unsafe { std::env::set_var("CASE_WAKE_INTERVAL_S", "86400") };
            let daily = std::panic::catch_unwind(wake_interval)
                .expect("CASE_WAKE_INTERVAL_S=86400 must not panic Duration::from_secs");
            assert_eq!(daily, Duration::from_secs(86400));
            assert!(daily > Duration::ZERO);

            // Overflow-risk: u64::MAX seconds must not panic / become ZERO.
            // Clamp to a finite Duration is required if from_secs would overflow;
            // passing through is ok only when from_secs holds.
            unsafe {
                std::env::set_var("CASE_WAKE_INTERVAL_S", &u64::MAX.to_string());
            }
            let huge = std::panic::catch_unwind(wake_interval)
                .expect("CASE_WAKE_INTERVAL_S=u64::MAX must not overflow Duration::from_secs");
            assert!(
                huge > Duration::ZERO,
                "CASE_WAKE_INTERVAL_S=u64::MAX must not yield {huge:?}"
            );
            assert!(
                huge >= Duration::from_secs(5),
                "CASE_WAKE_INTERVAL_S=u64::MAX must stay a sleepable interval, got {huge:?}"
            );

            // Control: an explicit 15s still parses so this is not "every interval is rejected".
            unsafe { std::env::set_var("CASE_WAKE_INTERVAL_S", "15") };
            assert_eq!(wake_interval(), Duration::from_secs(15));

            // Unset still defaults to 15s (docs / CASE_WAKE_INTERVAL_S).
            unsafe { std::env::remove_var("CASE_WAKE_INTERVAL_S") };
            assert_eq!(wake_interval(), Duration::from_secs(CASE_WAKE_INTERVAL_S));
            assert_eq!(wake_interval(), Duration::from_secs(15));
        });

        // The binary sleeps this Duration — a helper that clamps while main.rs
        // parses the env itself is not the lock. Do not require starting the loop.
        let main = include_str!("main.rs");
        assert!(
            main.contains("fleet_server::cases::wake_interval()"),
            "main.rs must sleep wake_interval(), not a raw CASE_WAKE_INTERVAL_S parse"
        );
        assert!(
            main.contains("tokio::time::sleep(interval)"),
            "main.rs case wake loop must sleep the clamped interval"
        );
    }
}
