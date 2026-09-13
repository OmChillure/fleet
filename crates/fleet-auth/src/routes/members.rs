//! Workspace members + email invites.

use axum::Json;
use axum::Router;
use axum::extract::FromRef;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, patch};
use axum_extra::extract::CookieJar;
use chrono::{Duration as ChronoDuration, Utc};
use fleet_db::repos::{users as user_repo, workspaces as workspace_repo};
use fleet_db::{WorkspaceInvite, WorkspaceMemberPublic};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::routes::auth::require_user;
use crate::state::AuthState;

pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    AuthState: FromRef<S>,
{
    Router::new()
        .route("/api/workspaces/{workspace_id}/members", get(list_members))
        .route(
            "/api/workspaces/{workspace_id}/members/{user_id}",
            patch(patch_member).delete(delete_member),
        )
        .route(
            "/api/workspaces/{workspace_id}/invites",
            get(list_invites).post(create_invite),
        )
        .route(
            "/api/workspaces/{workspace_id}/invites/{invite_id}",
            delete(revoke_invite),
        )
}

fn can_manage(role: &str) -> bool {
    matches!(role, "owner" | "admin")
}

/// Missing membership is 403 — same error PATCH/DELETE return from `require_member`.
fn require_workspace_role(role: Option<String>) -> AppResult<String> {
    role.ok_or_else(|| AppError::forbidden("not a member of this workspace"))
}

async fn require_member(
    state: &AuthState,
    jar: &CookieJar,
    workspace_id: Uuid,
) -> AppResult<(fleet_db::User, String)> {
    let user = require_user(state, jar).await?;
    let role =
        require_workspace_role(workspace_repo::user_role(&state.db, workspace_id, user.id).await?)?;
    Ok((user, role))
}

#[derive(Debug, Serialize)]
struct MemberListResponse {
    members: Vec<MemberJson>,
    you: MemberYou,
}

#[derive(Debug, Serialize)]
struct MemberJson {
    user_id: Uuid,
    email: String,
    display_name: Option<String>,
    role: String,
    created_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct MemberYou {
    user_id: Uuid,
    role: String,
}

impl From<&WorkspaceMemberPublic> for MemberJson {
    fn from(m: &WorkspaceMemberPublic) -> Self {
        Self {
            user_id: m.user_id,
            email: m.email.clone(),
            display_name: m.display_name.clone(),
            role: m.role.clone(),
            created_at: m.created_at,
        }
    }
}

async fn list_members(
    State(state): State<AuthState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<MemberListResponse>> {
    let (user, role) = require_member(&state, &jar, workspace_id).await?;
    let rows = workspace_repo::list_members(&state.db, workspace_id).await?;
    Ok(Json(MemberListResponse {
        members: rows.iter().map(MemberJson::from).collect(),
        you: MemberYou {
            user_id: user.id,
            role,
        },
    }))
}

#[derive(Debug, Deserialize)]
struct PatchMemberBody {
    role: String,
}

async fn patch_member(
    State(state): State<AuthState>,
    jar: CookieJar,
    Path((workspace_id, user_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<PatchMemberBody>,
) -> AppResult<Json<MemberJson>> {
    let (actor, actor_role) = require_member(&state, &jar, workspace_id).await?;
    if actor_role != "owner" {
        return Err(AppError::forbidden("only an owner can change roles"));
    }
    let new_role = workspace_repo::normalize_member_role(&body.role)
        .ok_or_else(|| AppError::bad_request("role must be owner, admin, or member"))?;
    if user_id == actor.id && new_role != "owner" {
        let owners = workspace_repo::count_owners(&state.db, workspace_id).await?;
        if owners <= 1 {
            return Err(AppError::bad_request("cannot demote the last owner"));
        }
    }
    let current = workspace_repo::user_role(&state.db, workspace_id, user_id)
        .await?
        .ok_or_else(|| AppError::not_found("member not found"))?;
    if current == "owner" && new_role != "owner" {
        let owners = workspace_repo::count_owners(&state.db, workspace_id).await?;
        if owners <= 1 {
            return Err(AppError::bad_request("cannot demote the last owner"));
        }
    }
    let ok = workspace_repo::set_member_role(&state.db, workspace_id, user_id, new_role).await?;
    if !ok {
        return Err(AppError::not_found("member not found"));
    }
    let rows = workspace_repo::list_members(&state.db, workspace_id).await?;
    let m = rows
        .iter()
        .find(|r| r.user_id == user_id)
        .ok_or_else(|| AppError::not_found("member not found"))?;
    Ok(Json(MemberJson::from(m)))
}

async fn delete_member(
    State(state): State<AuthState>,
    jar: CookieJar,
    Path((workspace_id, user_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<serde_json::Value>> {
    let (actor, actor_role) = require_member(&state, &jar, workspace_id).await?;
    let leaving = actor.id == user_id;
    if !leaving && !can_manage(&actor_role) {
        return Err(AppError::forbidden("not allowed to remove members"));
    }
    if !leaving && actor_role == "admin" {
        let target = workspace_repo::user_role(&state.db, workspace_id, user_id)
            .await?
            .ok_or_else(|| AppError::not_found("member not found"))?;
        if target == "owner" {
            return Err(AppError::forbidden("admins cannot remove an owner"));
        }
    }
    let target = workspace_repo::user_role(&state.db, workspace_id, user_id)
        .await?
        .ok_or_else(|| AppError::not_found("member not found"))?;
    if target == "owner" {
        let owners = workspace_repo::count_owners(&state.db, workspace_id).await?;
        if owners <= 1 {
            return Err(AppError::bad_request("cannot remove the last owner"));
        }
    }
    let ok = workspace_repo::remove_member(&state.db, workspace_id, user_id).await?;
    if !ok {
        return Err(AppError::not_found("member not found"));
    }
    Ok(Json(
        serde_json::json!({ "removed": true, "user_id": user_id }),
    ))
}

#[derive(Debug, Serialize)]
struct InviteJson {
    id: Uuid,
    email: String,
    role: String,
    created_at: chrono::DateTime<Utc>,
    expires_at: chrono::DateTime<Utc>,
}

impl From<&WorkspaceInvite> for InviteJson {
    fn from(i: &WorkspaceInvite) -> Self {
        Self {
            id: i.id,
            email: i.email_normalized.clone(),
            role: i.role.clone(),
            created_at: i.created_at,
            expires_at: i.expires_at,
        }
    }
}

#[derive(Debug, Serialize)]
struct InviteListResponse {
    invites: Vec<InviteJson>,
}

async fn list_invites(
    State(state): State<AuthState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<InviteListResponse>> {
    let (_, role) = require_member(&state, &jar, workspace_id).await?;
    if !can_manage(&role) {
        return Err(AppError::forbidden("not allowed to list invites"));
    }
    let rows = workspace_repo::list_open_invites(&state.db, workspace_id).await?;
    Ok(Json(InviteListResponse {
        invites: rows.iter().map(InviteJson::from).collect(),
    }))
}

#[derive(Debug, Deserialize)]
struct CreateInviteBody {
    email: String,
    #[serde(default)]
    role: Option<String>,
}

#[derive(Debug, Serialize)]
struct CreateInviteResponse {
    invite: InviteJson,
    added_existing_user: bool,
}

async fn create_invite(
    State(state): State<AuthState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<CreateInviteBody>,
) -> AppResult<(StatusCode, Json<CreateInviteResponse>)> {
    let (actor, actor_role) = require_member(&state, &jar, workspace_id).await?;
    if !can_manage(&actor_role) {
        return Err(AppError::forbidden("not allowed to invite"));
    }
    let email = body.email.trim().to_lowercase();
    if email.is_empty() || !email.contains('@') {
        return Err(AppError::bad_request("valid email is required"));
    }
    let role = workspace_repo::normalize_invite_role(body.role.as_deref().unwrap_or("member"))
        .ok_or_else(|| AppError::bad_request("invite role must be admin or member"))?;
    if actor_role == "admin" && role == "admin" {
        return Err(AppError::forbidden("admins can only invite members"));
    }

    if let Some(existing) = user_repo::find_by_email_normalized(&state.db, &email).await? {
        if workspace_repo::user_role(&state.db, workspace_id, existing.id)
            .await?
            .is_some()
        {
            return Err(AppError::conflict("user is already a member"));
        }
        let now = Utc::now();
        workspace_repo::add_member(&state.db, workspace_id, existing.id, role, now).await?;
        // Synthetic invite row so the UI can show “added”.
        return Ok((
            StatusCode::CREATED,
            Json(CreateInviteResponse {
                invite: InviteJson {
                    id: existing.id,
                    email,
                    role: role.to_string(),
                    created_at: now,
                    expires_at: now,
                },
                added_existing_user: true,
            }),
        ));
    }

    let now = Utc::now();
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    let token = hex::encode(bytes);
    let invite = WorkspaceInvite {
        id: Uuid::now_v7(),
        workspace_id,
        email_normalized: email,
        role: role.to_string(),
        token,
        invited_by: actor.id,
        created_at: now,
        expires_at: now + ChronoDuration::days(14),
        accepted_at: None,
        accepted_user_id: None,
        revoked_at: None,
    };
    let saved = workspace_repo::insert_invite(&state.db, &invite)
        .await
        .map_err(|e| {
            if e.is_unique_violation() {
                AppError::conflict("an open invite already exists for that email")
            } else {
                AppError::from(e)
            }
        })?;
    Ok((
        StatusCode::CREATED,
        Json(CreateInviteResponse {
            invite: InviteJson::from(&saved),
            added_existing_user: false,
        }),
    ))
}

async fn revoke_invite(
    State(state): State<AuthState>,
    jar: CookieJar,
    Path((workspace_id, invite_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<serde_json::Value>> {
    let (_, role) = require_member(&state, &jar, workspace_id).await?;
    if !can_manage(&role) {
        return Err(AppError::forbidden("not allowed to revoke invites"));
    }
    let ok = workspace_repo::revoke_invite(&state.db, workspace_id, invite_id, Utc::now()).await?;
    if !ok {
        return Err(AppError::not_found("invite not found"));
    }
    Ok(Json(
        serde_json::json!({ "revoked": true, "id": invite_id }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use http_body_util::BodyExt;
    use serde_json::{Value, json};
    use tower::ServiceExt;
    use uuid::Uuid;

    use crate::config::AuthConfig;
    use crate::state::AuthState;

    const TEST_PASSWORD: &str = "correct-horse";

    fn require_database_url() -> String {
        std::env::var("DATABASE_URL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                panic!(
                    "DATABASE_URL is required for this integration test. \
                     Start Postgres (just db-up) and export DATABASE_URL."
                )
            })
    }

    async fn app() -> axum::Router {
        let url = require_database_url();
        let db = fleet_db::connect_and_migrate_with(
            &url,
            &fleet_db::PoolConfig {
                max_connections: 5,
                min_connections: 0,
                acquire_timeout: Duration::from_secs(10),
                idle_timeout: Some(Duration::from_secs(30)),
                max_lifetime: None,
            },
        )
        .await
        .unwrap_or_else(|e| panic!("connect_and_migrate failed: {e}"));
        crate::router().with_state(AuthState {
            db,
            config: AuthConfig {
                jwt_secret: "dev-change-me-to-a-long-random-secret-at-least-32-chars".into(),
                access_ttl: Duration::from_secs(900),
                refresh_ttl: Duration::from_secs(604_800),
                cookie_secure: false,
                cookie_samesite: cookie::SameSite::Lax,
                signup_token: None,
                instance_admin_emails: Vec::new(),
            },
        })
    }

    #[derive(Default)]
    struct CookieJar {
        values: BTreeMap<String, String>,
    }

    impl CookieJar {
        fn absorb(&mut self, headers: &axum::http::HeaderMap) {
            for raw in headers
                .get_all(header::SET_COOKIE)
                .iter()
                .filter_map(|v| v.to_str().ok())
            {
                let nv = raw.split(';').next().unwrap_or(raw);
                if let Some((name, value)) = nv.split_once('=') {
                    if value.is_empty() {
                        self.values.remove(name.trim());
                    } else {
                        self.values
                            .insert(name.trim().to_string(), value.trim().to_string());
                    }
                }
            }
        }

        fn cookie_header(&self) -> Option<String> {
            if self.values.is_empty() {
                return None;
            }
            Some(
                self.values
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        }
    }

    struct HttpOut {
        status: StatusCode,
        headers: axum::http::HeaderMap,
        body: Value,
    }

    impl HttpOut {
        fn user_id(&self) -> &str {
            self.body["user"]["id"].as_str().expect("user.id")
        }

        fn workspace_id(&self) -> &str {
            self.body["default_workspace"]["id"]
                .as_str()
                .expect("default_workspace.id")
        }
    }

    async fn call(
        app: &axum::Router,
        method: &str,
        uri: &str,
        cookies: Option<&CookieJar>,
        body: Option<Value>,
    ) -> HttpOut {
        let mut builder = Request::builder().method(method).uri(uri);
        if body.is_some() {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
        }
        if let Some(h) = cookies.and_then(CookieJar::cookie_header) {
            builder = builder.header(header::COOKIE, h);
        }
        let req = builder
            .body(match body {
                Some(b) => Body::from(serde_json::to_vec(&b).unwrap()),
                None => Body::empty(),
            })
            .unwrap();
        let res = app.clone().oneshot(req).await.expect("oneshot");
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let body = if bytes.is_empty() {
            json!({})
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| json!({ "raw": String::from_utf8_lossy(&bytes) }))
        };
        HttpOut {
            status,
            headers,
            body,
        }
    }

    async fn signup_and_login(app: &axum::Router) -> (String, CookieJar, HttpOut) {
        let email = format!("leave.{uuid}@example.test", uuid = Uuid::new_v4());
        let created = call(
            app,
            "POST",
            "/api/auth/signup",
            None,
            Some(json!({ "email": email, "password": TEST_PASSWORD })),
        )
        .await;
        assert_eq!(
            created.status,
            StatusCode::CREATED,
            "signup: {}",
            created.body
        );
        let login_out = call(
            app,
            "POST",
            "/api/auth/login",
            None,
            Some(json!({ "email": email, "password": TEST_PASSWORD })),
        )
        .await;
        assert_eq!(
            login_out.status,
            StatusCode::OK,
            "login: {}",
            login_out.body
        );
        let mut jar = CookieJar::default();
        jar.absorb(&login_out.headers);
        (email, jar, login_out)
    }

    /// A must not PATCH B's membership. Leftover GET: B is still the sole owner.
    #[tokio::test]
    async fn patch_member_in_another_workspace_is_403() {
        let app = app().await;
        let (_a_email, a_jar, _a_login) = signup_and_login(&app).await;
        let (_b_email, b_jar, b_login) = signup_and_login(&app).await;
        let b_ws = b_login.workspace_id().to_string();
        let b_id = b_login.user_id().to_string();

        let patched = call(
            &app,
            "PATCH",
            &format!("/api/workspaces/{b_ws}/members/{b_id}"),
            Some(&a_jar),
            Some(json!({ "role": "member" })),
        )
        .await;
        assert_eq!(patched.status, StatusCode::FORBIDDEN, "{}", patched.body);

        let listed = call(
            &app,
            "GET",
            &format!("/api/workspaces/{b_ws}/members"),
            Some(&b_jar),
            None,
        )
        .await;
        assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
        assert_eq!(listed.body["you"]["role"].as_str(), Some("owner"));
        let members = listed.body["members"].as_array().expect("members");
        assert_eq!(members.len(), 1, "{}", listed.body);
        assert_eq!(members[0]["user_id"].as_str(), Some(b_id.as_str()));
        assert_eq!(members[0]["role"].as_str(), Some("owner"), "{}", listed.body);
    }

    #[tokio::test]
    async fn delete_member_in_another_workspace_is_403() {
        let app = app().await;
        let (_a_email, a_jar, _a_login) = signup_and_login(&app).await;
        let (_b_email, _b_jar, b_login) = signup_and_login(&app).await;
        let b_ws = b_login.workspace_id().to_string();
        let b_id = b_login.user_id().to_string();

        let deleted = call(
            &app,
            "DELETE",
            &format!("/api/workspaces/{b_ws}/members/{b_id}"),
            Some(&a_jar),
            None,
        )
        .await;
        assert_eq!(deleted.status, StatusCode::FORBIDDEN, "{}", deleted.body);
    }

    #[tokio::test]
    async fn list_members_in_another_workspace_is_403() {
        let app = app().await;
        let (_a_email, a_jar, _a_login) = signup_and_login(&app).await;
        let (_b_email, _b_jar, b_login) = signup_and_login(&app).await;
        let b_ws = b_login.workspace_id().to_string();

        let listed = call(
            &app,
            "GET",
            &format!("/api/workspaces/{b_ws}/members"),
            Some(&a_jar),
            None,
        )
        .await;
        assert_eq!(listed.status, StatusCode::FORBIDDEN, "{}", listed.body);
    }

    #[tokio::test]
    async fn create_invite_in_another_workspace_is_403() {
        let app = app().await;
        let (_a_email, a_jar, _a_login) = signup_and_login(&app).await;
        let (_b_email, b_jar, b_login) = signup_and_login(&app).await;
        let b_ws = b_login.workspace_id().to_string();
        let guest = format!("xws.{uuid}@example.test", uuid = Uuid::new_v4());

        let invited = call(
            &app,
            "POST",
            &format!("/api/workspaces/{b_ws}/invites"),
            Some(&a_jar),
            Some(json!({ "email": guest, "role": "member" })),
        )
        .await;
        assert_eq!(invited.status, StatusCode::FORBIDDEN, "{}", invited.body);

        let listed = call(
            &app,
            "GET",
            &format!("/api/workspaces/{b_ws}/invites"),
            Some(&b_jar),
            None,
        )
        .await;
        assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
        let invites = listed.body["invites"].as_array().expect("invites");
        assert!(
            invites.is_empty(),
            "B must have no open invite: {}",
            listed.body
        );
    }

    #[tokio::test]
    async fn unauthenticated_patch_member_is_401() {
        let app = app().await;
        let (_email, _jar, login) = signup_and_login(&app).await;
        let ws = login.workspace_id().to_string();
        let user_id = login.user_id().to_string();

        let out = call(
            &app,
            "PATCH",
            &format!("/api/workspaces/{ws}/members/{user_id}"),
            None,
            Some(json!({ "role": "member" })),
        )
        .await;
        assert_eq!(out.status, StatusCode::UNAUTHORIZED, "{}", out.body);
    }
}
