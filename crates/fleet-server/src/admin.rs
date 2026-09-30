//! Instance admin HTTP API. Cookie auth + `users.instance_admin`.

use std::collections::BTreeMap;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, patch};
use axum_extra::extract::CookieJar;
use chrono::Utc;
use fleet_auth::{AppError, AppResult, AuthState, require_instance_admin};
use fleet_db::User;
use fleet_db::repos::{admin as admin_repo, sessions as session_repo, users as user_repo};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::state::AppState;
use crate::worker_hosts::host_json_for_viewer;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/overview", get(http_overview))
        .route("/api/admin/users", get(http_users))
        .route("/api/admin/users/{user_id}", patch(http_patch_user))
        .route("/api/admin/workspaces", get(http_workspaces))
        .route("/api/admin/runners", get(http_runners))
        .route(
            "/api/admin/instance",
            get(http_instance).patch(http_patch_instance),
        )
        .route("/api/admin/work", get(http_work))
        .route("/api/admin/sessions", get(http_sessions))
}

fn auth_state(state: &AppState) -> AuthState {
    AuthState {
        db: state.db.clone(),
        config: state.auth.clone(),
    }
}

async fn require_admin(state: &AppState, jar: &CookieJar) -> AppResult<User> {
    require_instance_admin(&auth_state(state), jar).await
}

fn counts_map(rows: Vec<admin_repo::StatusCount>) -> BTreeMap<String, i64> {
    rows.into_iter().map(|r| (r.status, r.count)).collect()
}

fn sum_map(m: &BTreeMap<String, i64>) -> i64 {
    m.values().sum()
}

async fn http_overview(State(state): State<AppState>, jar: CookieJar) -> AppResult<Json<Value>> {
    let _ = require_admin(&state, &jar).await?;
    let users = user_repo::count_by_status(&state.db).await?;
    let mut users_by_status: BTreeMap<String, i64> = users.into_iter().collect();
    for key in ["active", "disabled", "pending"] {
        users_by_status.entry(key.to_string()).or_insert(0);
    }
    let instance_admins = user_repo::count_instance_admins(&state.db).await?;
    let workspaces = admin_repo::count_workspaces(&state.db).await?;
    let hosts = admin_repo::host_rollup(&state.db).await?;
    let runners: BTreeMap<String, i64> = BTreeMap::from([
        ("total".into(), hosts.total),
        ("online".into(), hosts.online),
        ("offline".into(), hosts.offline),
        ("pending".into(), hosts.pending),
        ("error".into(), hosts.error),
        ("disabled".into(), hosts.disabled),
        ("paused".into(), hosts.paused),
        ("drain".into(), hosts.drain),
        ("assignable".into(), hosts.assignable),
        ("updating".into(), hosts.updating),
    ]);
    let claims = counts_map(admin_repo::count_work_claims(&state.db).await?);
    let cases = counts_map(admin_repo::count_cases(&state.db).await?);
    let goals = counts_map(admin_repo::count_goals(&state.db).await?);
    let approvals = counts_map(admin_repo::count_approvals(&state.db).await?);
    let inbox = counts_map(admin_repo::count_inbox(&state.db).await?);
    let inbox_monitors = admin_repo::count_inbox_monitors_enabled(&state.db).await?;
    let unpublished = admin_repo::count_unpublished_events(&state.db).await?;
    let pending_invites = admin_repo::count_pending_invites(&state.db).await?;
    let connections = admin_repo::count_connections(&state.db).await?;
    let connected: i64 = connections
        .iter()
        .filter(|c| c.status == "connected")
        .map(|c| c.count)
        .sum();
    let sessions = counts_map(
        admin_repo::count_orch_sessions(&state.db)
            .await
            .unwrap_or_default(),
    );

    Ok(Json(json!({
        "users": {
            "total": sum_map(&users_by_status),
            "by_status": users_by_status,
            "instance_admins": instance_admins,
        },
        "workspaces": workspaces,
        "runners": runners,
        "sessions": sessions,
        "work_claims": claims,
        "cases": cases,
        "goals": goals,
        "approvals": approvals,
        "inbox": inbox,
        "inbox_monitors": inbox_monitors,
        "unpublished_events": unpublished,
        "pending_invites": pending_invites,
        "connected_integrations": connected,
    })))
}

async fn http_users(State(state): State<AppState>, jar: CookieJar) -> AppResult<Json<Value>> {
    let actor = require_admin(&state, &jar).await?;
    let users = admin_repo::list_users(&state.db).await?;
    Ok(Json(json!({
        "you": { "id": actor.id, "email": actor.email },
        "users": users,
    })))
}

#[derive(Debug, Deserialize)]
struct PatchUserBody {
    instance_admin: Option<bool>,
    status: Option<String>,
}

async fn http_patch_user(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(user_id): Path<Uuid>,
    Json(body): Json<PatchUserBody>,
) -> AppResult<Json<Value>> {
    let actor = require_admin(&state, &jar).await?;
    let target = user_repo::find_by_id(&state.db, user_id)
        .await?
        .ok_or_else(|| AppError::not_found("user not found"))?;

    if let Some(status) = body.status.as_deref() {
        match status {
            "active" | "disabled" => {}
            _ => {
                return Err(AppError::bad_request("status must be active or disabled"));
            }
        }
        if target.id == actor.id && status == "disabled" {
            return Err(AppError::bad_request("cannot disable yourself"));
        }
        if status == "disabled" && target.instance_admin {
            let admins = user_repo::count_instance_admins(&state.db).await?;
            if admins <= 1 {
                return Err(AppError::bad_request(
                    "cannot disable the last instance admin",
                ));
            }
        }
        let updated = user_repo::set_status(&state.db, user_id, status)
            .await?
            .ok_or_else(|| AppError::not_found("user not found"))?;
        if status == "disabled" {
            session_repo::revoke_all_for_user(&state.db, user_id, Utc::now()).await?;
        }
        tracing::info!(
            actor = %actor.id,
            user_id = %user_id,
            status,
            "instance admin set user status"
        );
        if body.instance_admin.is_none() {
            return Ok(Json(json!({ "user": fleet_db::UserPublic::from(updated) })));
        }
    }

    if let Some(want_admin) = body.instance_admin {
        if want_admin != target.instance_admin {
            if !want_admin {
                if state
                    .auth
                    .email_is_env_instance_admin(&target.email_normalized)
                {
                    return Err(AppError::bad_request(
                        "cannot demote an email listed in INSTANCE_ADMIN_EMAILS",
                    ));
                }
                let admins = user_repo::count_instance_admins(&state.db).await?;
                if admins <= 1 {
                    return Err(AppError::bad_request(
                        "cannot demote the last instance admin",
                    ));
                }
            }
            let updated = user_repo::set_instance_admin(&state.db, user_id, want_admin)
                .await?
                .ok_or_else(|| AppError::not_found("user not found"))?;
            tracing::info!(
                actor = %actor.id,
                user_id = %user_id,
                instance_admin = want_admin,
                "instance admin set instance_admin"
            );
            return Ok(Json(json!({ "user": fleet_db::UserPublic::from(updated) })));
        }
    }

    let latest = user_repo::find_by_id(&state.db, user_id)
        .await?
        .ok_or_else(|| AppError::not_found("user not found"))?;
    Ok(Json(json!({ "user": fleet_db::UserPublic::from(latest) })))
}

async fn http_workspaces(State(state): State<AppState>, jar: CookieJar) -> AppResult<Json<Value>> {
    let _ = require_admin(&state, &jar).await?;
    let workspaces = admin_repo::list_workspaces(&state.db).await?;
    Ok(Json(json!({ "workspaces": workspaces })))
}

async fn http_runners(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
) -> AppResult<Json<Value>> {
    let _ = require_admin(&state, &jar).await?;
    let viewer_loopback = crate::worker_hosts::origin_is_loopback(&headers);
    let names = admin_repo::workspace_names(&state.db).await?;
    let name_by_id: BTreeMap<Uuid, (String, String)> = names
        .into_iter()
        .map(|(id, slug, name)| (id, (slug, name)))
        .collect();
    let hosts = admin_repo::list_hosts(&state.db).await?;
    let runners: Vec<Value> = hosts
        .iter()
        .map(|h| {
            let public = host_json_for_viewer(&state, h, viewer_loopback);
            let mut v = serde_json::to_value(&public).unwrap_or_else(|_| json!({}));
            if let Some((slug, name)) = name_by_id.get(&h.workspace_id) {
                v["workspace_slug"] = json!(slug);
                v["workspace_name"] = json!(name);
            }
            v
        })
        .collect();
    Ok(Json(json!({ "runners": runners })))
}

fn env_set(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

async fn http_instance(State(state): State<AppState>, jar: CookieJar) -> AppResult<Json<Value>> {
    let _ = require_admin(&state, &jar).await?;
    let db_ok = state.db.acquire().await.is_ok();
    let git_sha = option_env!("FLEET_GIT_SHA")
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let packages = crate::install_routes::list_dist_packages(&state);
    let worker_public_base = std::env::var("WORKER_PUBLIC_BASE")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty());
    let oauth = json!({
        "github": state.integrations.github.is_some(),
        "gitlab": state.integrations.gitlab.is_some(),
        "gitea": state.integrations.gitea.is_some(),
        "gogs": !state.integrations.gogs_base_url.trim().is_empty(),
        "slack": state.integrations.slack.is_some(),
        "linear": state.integrations.linear.is_some(),
        "jira": state.integrations.jira.is_some(),
        "figma": state.integrations.figma.is_some(),
        "notion": state.integrations.notion.is_some(),
        "google_drive": state.integrations.google_drive.is_some(),
        "teams": state.integrations.teams.is_some(),
        "discord": state.integrations.discord.is_some(),
        "trello": state.integrations.trello.is_some(),
        "asana": state.integrations.asana.is_some(),
    });
    let connections = admin_repo::count_connections(&state.db).await?;
    let oauth_apps = admin_repo::count_oauth_apps(&state.db).await?;
    Ok(Json(json!({
        "health": {
            "status": if db_ok { "ok" } else { "degraded" },
            "db": if db_ok { "up" } else { "down" },
            "git_sha": git_sha,
            "service": "fleet-server",
        },
        "urls": {
            "api_public_base": state.integrations.api_public_base,
            "ui_public_base": state.integrations.ui_public_base,
            "worker_public_base": worker_public_base,
        },
        "signup_token_required": state.auth.signup_token.is_some(),
        "instance_admin_emails": state.auth.instance_admin_emails,
        "bootstrap_pair_enabled": env_set("FLEET_BOOTSTRAP_SECRET"),
        "fleet_max_agents": std::env::var("FLEET_MAX_AGENTS").ok(),
        "max_agents_cap": fleet_db::repos::instance_settings::get(&state.db)
            .await
            .map(|s| s.max_agents_cap)
            .unwrap_or(128),
        "providers": {
            "ollama": state.ollama.enabled,
            "grok": state.grok.enabled,
            "openai": state.openai.enabled && state.openai.api_key.is_some(),
            "claude": state.claude.enabled && state.claude.api_key.is_some(),
        },
        "oauth_configured": oauth,
        "connections": connections,
        "workspace_oauth_apps": oauth_apps,
        "packages": packages,
        "package_versions": {
            "fleet-host": crate::package_versions::fleet_host_version(),
            "fleet-driver": crate::package_versions::fleet_driver_version(),
        },
        "plane_git_sha": git_sha,
    })))
}

#[derive(Debug, Deserialize)]
struct PatchInstanceBody {
    max_agents_cap: Option<i32>,
}

async fn http_patch_instance(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(body): Json<PatchInstanceBody>,
) -> AppResult<Json<Value>> {
    let _ = require_admin(&state, &jar).await?;
    let Some(cap) = body.max_agents_cap else {
        return Err(AppError::bad_request("max_agents_cap required"));
    };
    if !(1..=1024).contains(&cap) {
        return Err(AppError::bad_request("max_agents_cap must be 1–1024"));
    }
    let row = fleet_db::repos::instance_settings::set_max_agents_cap(&state.db, cap).await?;
    Ok(Json(json!({ "max_agents_cap": row.max_agents_cap })))
}

async fn http_work(State(state): State<AppState>, jar: CookieJar) -> AppResult<Json<Value>> {
    let _ = require_admin(&state, &jar).await?;
    Ok(Json(json!({
        "work_claims": counts_map(admin_repo::count_work_claims(&state.db).await?),
        "cases": counts_map(admin_repo::count_cases(&state.db).await?),
        "goals": counts_map(admin_repo::count_goals(&state.db).await?),
        "approvals": counts_map(admin_repo::count_approvals(&state.db).await?),
        "inbox": counts_map(admin_repo::count_inbox(&state.db).await?),
        "inbox_monitors": admin_repo::count_inbox_monitors_enabled(&state.db).await?,
        "unpublished_events": admin_repo::count_unpublished_events(&state.db).await?,
        "sessions": counts_map(
            admin_repo::count_orch_sessions(&state.db).await.unwrap_or_default()
        ),
        "tasks": counts_map(
            admin_repo::count_orch_tasks(&state.db).await.unwrap_or_default()
        ),
        "runs": counts_map(
            admin_repo::count_orch_runs(&state.db).await.unwrap_or_default()
        ),
    })))
}

async fn http_sessions(State(state): State<AppState>, jar: CookieJar) -> AppResult<Json<Value>> {
    let _ = require_admin(&state, &jar).await?;
    let sessions = admin_repo::list_active_sessions(&state.db).await?;
    Ok(Json(json!({ "sessions": sessions })))
}
