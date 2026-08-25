//! Partners disclaimer ack (timestamp only).
//!
//! Acknowledging does **not** enable lending, borrowing, enqueue, or
//! assignable partner runners.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use chrono::Utc;
use fleet_auth::{AppError, AppResult, AuthState, require_user};
use fleet_db::PartnersDisclaimerAck;
use fleet_db::repos::workspaces as workspace_repo;
use uuid::Uuid;

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/workspaces/{workspace_id}/partners/disclaimer",
        get(get_disclaimer).post(post_disclaimer),
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

async fn get_disclaimer(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<PartnersDisclaimerAck>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let accepted_at =
        workspace_repo::partners_disclaimer_accepted_at(&state.db, workspace_id).await?;
    Ok(Json(PartnersDisclaimerAck::from_accepted_at(accepted_at)))
}

async fn post_disclaimer(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<PartnersDisclaimerAck>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let accepted_at =
        workspace_repo::accept_partners_disclaimer(&state.db, workspace_id, Utc::now()).await?;
    Ok(Json(PartnersDisclaimerAck::from_accepted_at(Some(
        accepted_at,
    ))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use serde_json::Value;

    // catalog:unit-cloud-partners-helpers — disclaimer ack is a timestamp
    // only: first-write-wins; ack ≠ assignable; no lend/enqueue.

    fn ts(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .expect("rfc3339")
            .with_timezone(&Utc)
    }

    /// Same mapping GET/POST use after the repo read or first-write-wins write.
    fn disclaimer_ack(accepted_at: Option<DateTime<Utc>>) -> PartnersDisclaimerAck {
        PartnersDisclaimerAck::from_accepted_at(accepted_at)
    }

    fn ack_json(accepted_at: Option<DateTime<Utc>>) -> Value {
        serde_json::to_value(disclaimer_ack(accepted_at)).expect("serialize")
    }

    fn production_src() -> &'static str {
        let src = include_str!("partners.rs");
        match src.find("#[cfg(test)]") {
            Some(i) => &src[..i],
            None => src,
        }
    }

    #[test]
    fn disclaimer_ack_is_timestamp_only() {
        let missing = ack_json(None);
        assert_eq!(missing["accepted"], false);
        assert!(missing["accepted_at"].is_null());
        let missing_obj = missing.as_object().expect("object");
        assert_eq!(missing_obj.len(), 2, "ack fields must be timestamp only");

        let at = ts("2026-08-20T12:00:00Z");
        let recorded = ack_json(Some(at));
        assert_eq!(recorded["accepted"], true);
        assert_eq!(recorded["accepted_at"], "2026-08-20T12:00:00Z");
        let recorded_obj = recorded.as_object().expect("object");
        assert_eq!(recorded_obj.len(), 2);
        assert_eq!(
            recorded_obj.keys().collect::<Vec<_>>(),
            vec!["accepted", "accepted_at"]
        );
        for key in [
            "assignable",
            "lending",
            "borrowing",
            "enqueue",
            "lend",
            "borrow",
            "credits",
            "balance",
            "pool",
        ] {
            assert!(
                !recorded_obj.contains_key(key),
                "ack must not expose {key}: {recorded}"
            );
        }
    }

    #[test]
    fn disclaimer_ack_first_write_wins_echoes_stored_timestamp() {
        // post_disclaimer sends Utc::now() as the candidate, then wraps the
        // DateTime accept_partners_disclaimer returns (COALESCE first write).
        let first = ts("2026-08-20T12:00:00Z");
        let later = ts("2026-08-20T13:00:00Z");
        let posted = disclaimer_ack(Some(first));
        let again = disclaimer_ack(Some(first));
        assert!(posted.accepted);
        assert_eq!(posted.accepted_at, Some(first));
        assert_eq!(again.accepted_at, posted.accepted_at);
        assert_ne!(again.accepted_at, Some(later));

        let src = production_src();
        assert!(
            src.contains("accept_partners_disclaimer(&state.db, workspace_id, Utc::now())"),
            "POST candidate is now(); first-write-wins is the repo COALESCE"
        );
        assert!(
            src.contains("PartnersDisclaimerAck::from_accepted_at(Some("),
            "POST must echo the stored timestamp, not a second clock read"
        );
    }

    #[test]
    fn disclaimer_ack_does_not_enable_lend_enqueue_or_assignable() {
        for accepted_at in [None, Some(ts("2026-08-20T12:00:00Z"))] {
            let ack = disclaimer_ack(accepted_at);
            let body = ack_json(accepted_at);
            assert_eq!(ack.accepted, accepted_at.is_some());
            assert_eq!(ack.accepted_at, accepted_at);
            assert!(
                body.get("assignable").is_none()
                    && body.get("lending").is_none()
                    && body.get("enqueue").is_none(),
                "ack={:?} must not flip lend/enqueue/assignable: {body}",
                ack.accepted
            );
        }
    }

    #[test]
    fn disclaimer_handlers_have_no_assignable_or_lend_path() {
        let _ = router();
        let src = production_src();

        assert!(src.contains("/api/workspaces/{workspace_id}/partners/disclaimer"));
        assert!(src.contains("get(get_disclaimer).post(post_disclaimer)"));
        assert_eq!(
            src.matches(".route(").count(),
            1,
            "partners router must only expose the disclaimer ack"
        );
        assert!(!src.contains("put(") && !src.contains("patch(") && !src.contains("delete("));

        let repo_calls: Vec<&str> = src
            .lines()
            .filter(|line| line.contains("workspace_repo::"))
            .map(str::trim)
            .collect();
        assert!(
            repo_calls
                .iter()
                .any(|l| l.contains("workspace_repo::user_role")),
            "membership check missing: {repo_calls:?}"
        );
        assert!(
            repo_calls
                .iter()
                .any(|l| l.contains("workspace_repo::partners_disclaimer_accepted_at")),
            "GET must only read the timestamp: {repo_calls:?}"
        );
        assert!(
            repo_calls
                .iter()
                .any(|l| l.contains("workspace_repo::accept_partners_disclaimer")),
            "POST must only persist the timestamp: {repo_calls:?}"
        );
        assert_eq!(
            repo_calls.len(),
            3,
            "no extra workspace_repo calls: {repo_calls:?}"
        );

        for needle in [
            "list_assignable",
            "select_assignable",
            "set_assignable",
            "worker_hosts",
            "create_pending",
            "enqueue(",
            "lend(",
            "enable_lend",
        ] {
            assert!(!src.contains(needle), "ack path must not {needle}:\n{src}");
        }
    }
}
