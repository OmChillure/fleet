//! Worker-authenticated plane I/O. Tokens stay here.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::auth::WorkerAuth;
use super::handlers::{ApiError, err, map_store};
use super::state::AppState;
use crate::sandbox::share::ChangesRequest;
use crate::sandbox::team;
use crate::sandbox::team_chat::{self, TeamTalk};

#[derive(Debug, Deserialize)]
pub struct WorkerIoBody {
    pub op: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub operation: Option<String>,
    #[serde(default)]
    pub args: Option<Value>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub all: Option<bool>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub targets: Option<Vec<String>>,
}

pub async fn post_worker_io(
    State(state): State<AppState>,
    worker: WorkerAuth,
    Path((project_id, session_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<WorkerIoBody>,
) -> Result<Json<Value>, ApiError> {
    let session = state
        .store
        .authenticate_worker_session(session_id, &worker.token)
        .await
        .map_err(map_store)?
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "invalid worker session/token"))?;
    if session.project_id != project_id {
        return Err(err(StatusCode::NOT_FOUND, "session not in project"));
    }
    let task = state
        .store
        .get_task(project_id, session.board_task_id)
        .await
        .map_err(map_store)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "task not found"))?;
    let talk = TeamTalk::from_spawn(
        state.tick.data_dir(),
        project_id,
        session.run_id,
        task.plan_task_id.clone(),
        session.worker_profile.clone(),
    );
    let work = team::session_dir(state.tick.data_dir(), project_id, session.id);

    match body.op.trim() {
        "integration" | "use_integration" => {
            let Some(inv) = state.tick.integrations().get() else {
                return Ok(Json(json!({
                    "ok": false,
                    "summary": "no control-plane integration proxy",
                })));
            };
            let provider = body
                .provider
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| err(StatusCode::BAD_REQUEST, "provider required"))?;
            let operation = body
                .operation
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| err(StatusCode::BAD_REQUEST, "operation required"))?;
            let args = body.args.unwrap_or_else(|| json!({}));
            let res = inv
                .invoke(project_id, provider.to_string(), operation.to_string(), args)
                .await;
            Ok(Json(json!({
                "ok": res.ok,
                "summary": res.summary,
                "data": res.data,
            })))
        }
        "team_chat_post" | "send_message" => {
            let to = body.to.as_deref().unwrap_or("*");
            let text = body
                .body
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| err(StatusCode::BAD_REQUEST, "body required"))?;
            let reply_to = body
                .reply_to
                .as_deref()
                .and_then(|s| Uuid::parse_str(s.trim()).ok());
            let kind = body
                .kind
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(if reply_to.is_some() { "reply" } else { "say" });
            let msg = team_chat::post_message(&talk, to, kind, text, reply_to)
                .await
                .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
            team_chat::copy_team_chat_into(&work, &talk).await;
            Ok(Json(json!({
                "ok": true,
                "id": msg.id,
                "seq": msg.seq,
                "to": msg.to,
                "kind": msg.kind,
            })))
        }
        "team_chat_list" | "read_messages" => {
            let all = body.all.unwrap_or(true);
            let msgs = if all {
                team_chat::list_messages(&talk).await
            } else {
                team_chat::unread_for(&talk).await
            };
            if !all {
                team_chat::mark_seen_through(&talk, &msgs).await;
            }
            Ok(Json(json!({ "ok": true, "messages": msgs })))
        }
        "request_changes" => {
            let reason = body
                .reason
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| err(StatusCode::BAD_REQUEST, "reason required"))?;
            let req = ChangesRequest {
                targets: body.targets.unwrap_or_default(),
                reason: reason.to_string(),
            };
            crate::sandbox::share::write_changes_request(&work, &req)
                .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
            let to = req.targets.first().map(|s| s.as_str()).unwrap_or("*");
            let _ = team_chat::post_message(
                &talk,
                to,
                "ask",
                &format!("Changes requested: {reason}"),
                None,
            )
            .await;
            Ok(Json(json!({ "ok": true, "targets": req.targets })))
        }
        other => Err(err(
            StatusCode::BAD_REQUEST,
            format!("unknown op `{other}`"),
        )),
    }
}
