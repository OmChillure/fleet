//! Workspace personas — agent role configs (no LLM calls).

use axum::Json;
use axum::Router;
use axum::extract::{FromRef, Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum_extra::extract::CookieJar;
use chrono::Utc;
use fleet_db::repos::{personas as persona_repo, workspaces as workspace_repo};
use fleet_db::{
    PersonaPublic,
    repos::personas::{NewPersona, UpdatePersona},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::routes::auth::require_user;
use crate::state::AuthState;

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub status: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PersonaListResponse {
    pub workspace_id: String,
    pub personas: Vec<PersonaPublic>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePersonaRequest {
    pub name: String,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub role_prompt: Option<String>,
    pub worker_profile: Option<String>,
    pub model_preference: Option<String>,
    pub temperature: Option<f64>,
    pub max_budget_usd: Option<f64>,
    pub tools: Option<Value>,
    pub is_default: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct PatchPersonaRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub role_prompt: Option<String>,
    pub worker_profile: Option<String>,
    pub model_preference: Option<Option<String>>,
    pub temperature: Option<Option<f64>>,
    pub max_budget_usd: Option<Option<f64>>,
    pub tools: Option<Value>,
    pub is_default: Option<bool>,
    pub status: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SeedResponse {
    pub inserted: usize,
    pub skipped: usize,
}

/// Static starter catalog (product vision).
struct Starter {
    slug: &'static str,
    name: &'static str,
    description: &'static str,
    icon: &'static str,
    worker_profile: &'static str,
    role_prompt: &'static str,
    model_preference: &'static str,
    tools: &'static [&'static str],
    is_default: bool,
}

fn starters() -> &'static [Starter] {
    &[
        Starter {
            slug: "software-engineer",
            name: "Software engineer",
            description: "Implement tickets, write tests, open PRs.",
            icon: "code",
            worker_profile: "coding",
            role_prompt: "You are a careful software engineer. Prefer small, reviewable changes, tests, and clear commit messages.",
            model_preference: "auto",
            tools: &["git", "shell", "editor"],
            is_default: true,
        },
        Starter {
            slug: "sales",
            name: "Sales",
            description: "Outreach drafts, CRM hygiene, pipeline notes.",
            icon: "sales",
            worker_profile: "ops",
            role_prompt: "You are a sales operations specialist. Be concise, CRM-friendly, and never invent customer facts.",
            model_preference: "auto",
            tools: &["crm", "docs"],
            is_default: false,
        },
        Starter {
            slug: "marketing",
            name: "Marketing",
            description: "Campaign briefs, copy drafts, channel plans.",
            icon: "megaphone",
            worker_profile: "ops",
            role_prompt: "You are a marketing generalist. Match brand tone; label assumptions; prefer measurable goals.",
            model_preference: "auto",
            tools: &["docs", "analytics"],
            is_default: false,
        },
        Starter {
            slug: "customer-support",
            name: "Customer support",
            description: "Triage tickets, draft replies, escalate blockers.",
            icon: "support",
            worker_profile: "ops",
            role_prompt: "You are customer support. Empathetic, accurate, never invent product capabilities.",
            model_preference: "auto",
            tools: &["tickets", "docs"],
            is_default: false,
        },
        Starter {
            slug: "analyst",
            name: "Analyst",
            description: "Data pulls, summaries, decision memos.",
            icon: "chart",
            worker_profile: "analysis",
            role_prompt: "You are a data analyst. Cite sources, show uncertainty, prefer tables and clear recommendations.",
            model_preference: "auto",
            tools: &["sql", "docs", "charts"],
            is_default: false,
        },
        Starter {
            slug: "devops",
            name: "DevOps",
            description: "CI, infra changes, incident runbooks.",
            icon: "ops",
            worker_profile: "coding",
            role_prompt: "You are a DevOps engineer. Prefer reversible changes, observability, and safe rollouts.",
            model_preference: "auto",
            tools: &["git", "shell", "ci"],
            is_default: false,
        },
        Starter {
            slug: "product-discovery",
            name: "Product discovery",
            description: "Research notes, problem framing, lightweight specs.",
            icon: "compass",
            worker_profile: "ops",
            role_prompt: "You are a product discovery partner. Separate problem/solution; list risks and open questions.",
            model_preference: "auto",
            tools: &["docs", "research"],
            is_default: false,
        },
        Starter {
            slug: "content-docs",
            name: "Content / docs",
            description: "Docs, changelogs, internal write-ups.",
            icon: "docs",
            worker_profile: "ops",
            role_prompt: "You are a technical writer. Clear structure, accurate terminology, scannable headings.",
            model_preference: "auto",
            tools: &["docs", "git"],
            is_default: false,
        },
    ]
}

/// Roster label: trim, then reject empty / over 80 (create and PATCH).
fn validated_persona_name(raw: &str) -> AppResult<String> {
    let name = raw.trim().to_string();
    if name.is_empty() {
        return Err(AppError::bad_request("name is required"));
    }
    if name.len() > 80 {
        return Err(AppError::bad_request("name is too long"));
    }
    Ok(name)
}

/// Same 32k family as spec / policy_md / chat — do not persist over-cap text.
const PERSONA_TEXT_MAX: usize = 32_000;

fn validated_persona_text(field: &str, raw: String) -> AppResult<String> {
    if raw.len() > PERSONA_TEXT_MAX {
        return Err(AppError::bad_request(format!(
            "{field} too long (max 32k characters)"
        )));
    }
    Ok(raw)
}

fn slugify(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut prev_dash = false;
    for c in raw.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "persona".into()
    } else {
        trimmed.chars().take(48).collect()
    }
}

async fn require_member(
    state: &AuthState,
    jar: &CookieJar,
    workspace_id: Uuid,
) -> AppResult<(fleet_db::User, fleet_db::WorkspaceWithRole)> {
    let user = require_user(state, jar).await?;
    let ws = workspace_repo::find_for_user(&state.db, workspace_id, user.id)
        .await?
        .ok_or_else(|| AppError::forbidden("not a member of this workspace"))?;
    Ok((user, ws))
}

pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    AuthState: FromRef<S>,
{
    Router::new()
        .route(
            "/api/workspaces/{workspace_id}/personas",
            get(list_personas).post(create_persona),
        )
        .route(
            "/api/workspaces/{workspace_id}/personas/seed-starters",
            post(seed_starters),
        )
        .route(
            "/api/workspaces/{workspace_id}/personas/{persona_id}",
            get(get_persona).patch(patch_persona).delete(delete_persona),
        )
}

async fn list_personas(
    State(state): State<AuthState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<PersonaListResponse>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let status = q.status.as_deref();
    let rows = persona_repo::list_for_workspace(&state.db, workspace_id, status).await?;
    Ok(Json(PersonaListResponse {
        workspace_id: workspace_id.to_string(),
        personas: rows.into_iter().map(PersonaPublic::from).collect(),
    }))
}

async fn get_persona(
    State(state): State<AuthState>,
    jar: CookieJar,
    Path((workspace_id, persona_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<PersonaPublic>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let p = persona_repo::get(&state.db, workspace_id, persona_id)
        .await?
        .ok_or_else(|| AppError::not_found("persona not found"))?;
    Ok(Json(PersonaPublic::from(p)))
}

async fn create_persona(
    State(state): State<AuthState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<CreatePersonaRequest>,
) -> AppResult<(StatusCode, Json<PersonaPublic>)> {
    let (user, _) = require_member(&state, &jar, workspace_id).await?;
    let name = validated_persona_name(&body.name)?;
    let description = validated_persona_text(
        "description",
        body.description.unwrap_or_default().trim().to_string(),
    )?;
    let role_prompt = validated_persona_text("role_prompt", body.role_prompt.unwrap_or_default())?;
    let slug = body
        .slug
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(slugify)
        .unwrap_or_else(|| slugify(&name));
    let now = Utc::now();
    let new = NewPersona {
        id: Uuid::now_v7(),
        workspace_id,
        slug,
        name,
        description,
        icon: body.icon.unwrap_or_else(|| "bot".into()).trim().to_string(),
        role_prompt,
        worker_profile: body
            .worker_profile
            .unwrap_or_else(|| "coding".into())
            .trim()
            .to_string(),
        model_preference: body
            .model_preference
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        temperature: body.temperature,
        max_budget_usd: body.max_budget_usd,
        tools: body.tools.unwrap_or_else(|| json!([])),
        is_starter: false,
        is_default: body.is_default.unwrap_or(false),
        created_by: Some(user.id),
        created_at: now,
    };
    let p = persona_repo::insert(&state.db, &new).await.map_err(|e| {
        if e.is_unique_violation() {
            AppError::conflict("persona slug already exists in this workspace")
        } else {
            AppError::from(e)
        }
    })?;
    Ok((StatusCode::CREATED, Json(PersonaPublic::from(p))))
}

async fn patch_persona(
    State(state): State<AuthState>,
    jar: CookieJar,
    Path((workspace_id, persona_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<PatchPersonaRequest>,
) -> AppResult<Json<PersonaPublic>> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    if let Some(ref s) = body.status {
        if s != "active" && s != "archived" {
            return Err(AppError::bad_request("status must be active or archived"));
        }
    }
    let name = match body.name {
        Some(raw) => Some(validated_persona_name(&raw)?),
        None => None,
    };
    let description = match body.description {
        Some(raw) => Some(validated_persona_text("description", raw)?),
        None => None,
    };
    let role_prompt = match body.role_prompt {
        Some(raw) => Some(validated_persona_text("role_prompt", raw)?),
        None => None,
    };
    let patch = UpdatePersona {
        name,
        description,
        icon: body.icon,
        role_prompt,
        worker_profile: body.worker_profile,
        model_preference: body.model_preference,
        temperature: body.temperature,
        max_budget_usd: body.max_budget_usd,
        tools: body.tools,
        is_default: body.is_default,
        status: body.status,
    };
    let p = persona_repo::update(&state.db, workspace_id, persona_id, &patch, Utc::now())
        .await?
        .ok_or_else(|| AppError::not_found("persona not found"))?;
    Ok(Json(PersonaPublic::from(p)))
}

async fn delete_persona(
    State(state): State<AuthState>,
    jar: CookieJar,
    Path((workspace_id, persona_id)): Path<(Uuid, Uuid)>,
) -> AppResult<StatusCode> {
    let _ = require_member(&state, &jar, workspace_id).await?;
    let ok = persona_repo::archive(&state.db, workspace_id, persona_id, Utc::now()).await?;
    if !ok {
        return Err(AppError::not_found("persona not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn seed_starters(
    State(state): State<AuthState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<SeedResponse>> {
    let (user, _) = require_member(&state, &jar, workspace_id).await?;
    let now = Utc::now();
    let mut inserted = 0usize;
    let mut skipped = 0usize;
    for s in starters() {
        let tools = Value::Array(s.tools.iter().map(|t| json!(t)).collect());
        let new = NewPersona {
            id: Uuid::now_v7(),
            workspace_id,
            slug: s.slug.into(),
            name: s.name.into(),
            description: s.description.into(),
            icon: s.icon.into(),
            role_prompt: s.role_prompt.into(),
            worker_profile: s.worker_profile.into(),
            model_preference: Some(s.model_preference.into()),
            temperature: None,
            max_budget_usd: None,
            tools,
            is_starter: true,
            // Avoid fighting an existing workspace default; user can mark default in UI.
            is_default: false,
            created_by: Some(user.id),
            created_at: now,
        };
        let _ = s.is_default;
        match persona_repo::insert_if_absent(&state.db, &new).await {
            Ok(true) => inserted += 1,
            Ok(false) => skipped += 1,
            Err(e) => return Err(AppError::from(e)),
        }
    }
    Ok(Json(SeedResponse { inserted, skipped }))
}

#[cfg(test)]
mod tests {
    use super::{slugify, starters};

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Software Engineer"), "software-engineer");
        assert_eq!(slugify("  Content / docs  "), "content-docs");
        assert_eq!(slugify("!!!"), "persona");
    }

    #[test]
    fn starter_catalog_has_eight_roles() {
        let slugs: Vec<&str> = starters().iter().map(|s| s.slug).collect();
        assert_eq!(slugs.len(), 8);
        assert!(slugs.contains(&"software-engineer"));
        assert!(slugs.contains(&"content-docs"));
        assert_eq!(starters().iter().filter(|s| s.is_default).count(), 1);
    }

    use std::collections::BTreeMap;
    use std::time::Duration;

    use crate::{AuthConfig, AuthState};
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use http_body_util::BodyExt;
    use serde_json::{Value, json};
    use tower::ServiceExt;
    use uuid::Uuid;

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

    fn test_auth_config() -> AuthConfig {
        let jwt_secret = std::env::var("JWT_SECRET")
            .ok()
            .filter(|s| s.len() >= 32)
            .unwrap_or_else(|| "dev-change-me-to-a-long-random-secret-at-least-32-chars".into());
        AuthConfig {
            jwt_secret,
            access_ttl: Duration::from_secs(900),
            refresh_ttl: Duration::from_secs(604_800),
            cookie_secure: false,
            cookie_samesite: cookie::SameSite::Lax,
            signup_token: None,
        }
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
            config: test_auth_config(),
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
        fn error(&self) -> &str {
            self.body["error"].as_str().unwrap_or("")
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

    async fn signup_and_login(app: &axum::Router) -> (CookieJar, HttpOut) {
        let email = format!("persona.{uuid}@example.test", uuid = Uuid::new_v4());
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
        (jar, login_out)
    }

    fn listed_ids(out: &HttpOut) -> Vec<String> {
        out.body["personas"]
            .as_array()
            .expect("personas array")
            .iter()
            .filter_map(|p| p["id"].as_str().map(str::to_string))
            .collect()
    }

    /// GET/PATCH/DELETE of a garbage or unknown persona id must 400/404
    /// and must not insert a row (assign-as-default included).
    #[tokio::test]
    async fn unknown_persona_id_is_rejected_on_assign_and_not_persisted() {
        let app = app().await;
        let (jar, login) = signup_and_login(&app).await;
        let ws_id = login.body["default_workspace"]["id"]
            .as_str()
            .expect("default_workspace.id");

        let created = call(
            &app,
            "POST",
            &format!("/api/workspaces/{ws_id}/personas"),
            Some(&jar),
            Some(json!({ "name": "Known persona" })),
        )
        .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        let known_id = created.body["id"].as_str().expect("created id").to_string();

        let garbage = "not-a-persona-id";
        let unknown = Uuid::now_v7();
        let assign_body = json!({
            "name": "injected-garbage",
            "is_default": true,
        });

        for method in ["GET", "PATCH", "DELETE"] {
            let uri = format!("/api/workspaces/{ws_id}/personas/{garbage}");
            let body = (method == "PATCH").then(|| assign_body.clone());
            let out = call(&app, method, &uri, Some(&jar), body).await;
            assert_eq!(
                out.status,
                StatusCode::BAD_REQUEST,
                "{method} garbage persona id: {}",
                out.body
            );
        }

        for method in ["GET", "PATCH", "DELETE"] {
            let uri = format!("/api/workspaces/{ws_id}/personas/{unknown}");
            let body = (method == "PATCH").then(|| assign_body.clone());
            let out = call(&app, method, &uri, Some(&jar), body).await;
            assert_eq!(
                out.status,
                StatusCode::NOT_FOUND,
                "{method} unknown persona id: {}",
                out.body
            );
            assert!(
                out.error().contains("persona not found"),
                "{method} should say persona not found, got {:?}",
                out.error()
            );
        }

        let listed = call(
            &app,
            "GET",
            &format!("/api/workspaces/{ws_id}/personas"),
            Some(&jar),
            None,
        )
        .await;
        assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
        let ids = listed_ids(&listed);
        assert!(
            ids.contains(&known_id),
            "known persona must remain: {ids:?}"
        );
        assert!(
            !ids.contains(&unknown.to_string()),
            "unknown persona id must not persist: {ids:?}"
        );
        assert!(
            ids.iter()
                .all(|id| id != garbage && Uuid::parse_str(id).is_ok()),
            "list must not contain garbage id: {ids:?}"
        );

        let archived = call(
            &app,
            "GET",
            &format!("/api/workspaces/{ws_id}/personas?status=archived"),
            Some(&jar),
            None,
        )
        .await;
        assert_eq!(archived.status, StatusCode::OK, "{}", archived.body);
        let archived_ids = listed_ids(&archived);
        assert!(
            !archived_ids.contains(&unknown.to_string()),
            "unknown persona must not persist as archived: {archived_ids:?}"
        );
        assert!(
            !archived_ids.iter().any(|id| id == garbage),
            "garbage persona must not persist as archived: {archived_ids:?}"
        );
    }

    fn production_src() -> &'static str {
        let src = include_str!("personas.rs");
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

    fn assert_no_llm_call(src: &str, label: &str) {
        let lower = src.to_ascii_lowercase();
        for needle in [
            "chat/completions",
            "/v1/chat",
            "/v1/responses",
            "complete_text",
            "chat_complete",
            "execute_tool",
            "launch_agent_task(",
            "launch_agent_plan(",
            "generate_prompt",
            "rewrite_prompt",
            "suggest_prompt",
            "grok::",
            "openai_chat",
            "openai_completions",
            "anthropic_chat",
            "ollama_chat",
            "ollama.com/api/generate",
            "api.x.ai",
            "api.openai.com",
            "api.anthropic.com",
            "async-openai",
            "async_openai",
            "xai::",
            "invoke_llm",
            "stream_complete",
        ] {
            assert!(
                !lower.contains(needle),
                "{label} must not call an LLM ({needle}):\n{src}"
            );
        }
    }

    fn assert_no_outbound_llm_client(src: &str, label: &str) {
        let lower = src.to_ascii_lowercase();
        for needle in [
            "reqwest",
            "hyper::client",
            "hyper_util",
            "std::process",
            "command::new",
            "tokio::process",
        ] {
            assert!(
                !lower.contains(needle),
                "{label} must not open an HTTP client or spawn an LLM CLI ({needle}):\n{src}"
            );
        }
        assert_no_llm_call(src, label);
    }

    fn assert_spawn_does_not_read_workspace_role_prompt(label: &str, src: &str) {
        // Docs §6.7 / §17: prompt/model/budget/tools are stored only.
        // Spawn resolving agency-agents roster files is fine; reading the
        // workspace `role_prompt` column is not. If spawn starts to, fail here.
        // tick_ops.rs is where a SELECT role_prompt FROM personas would live.
        assert!(
            !src.contains("find_workspace_persona"),
            "{label} must not read workspace persona role_prompt \
             (find_workspace_persona); docs §6.7 — this card is the lock"
        );
        assert!(
            !src.contains("role_prompt"),
            "{label} must not read role_prompt; docs §6.7 — this card is the lock"
        );
        assert!(
            !src.to_ascii_lowercase().contains("from personas"),
            "{label} must not SELECT workspace personas; docs §6.7"
        );
        assert!(
            !src.contains("SELECT name, role_prompt")
                && !src
                    .to_ascii_lowercase()
                    .contains("select name, role_prompt"),
            "{label} must not SELECT role_prompt from personas; docs §6.7"
        );
    }

    /// Docs §6.7: Agents & personas is seed/create/patch/archive only.
    /// No LLM is invoked. Worker spawn must not read `role_prompt` —
    /// if it starts to, this test is the lock.
    #[test]
    fn personas_page_does_not_call_llm() {
        for (label, src) in [
            (
                "orch/spawn.rs",
                include_str!("../../../fleet-orchestrator/src/orch/spawn.rs"),
            ),
            (
                "orch/tick.rs",
                include_str!("../../../fleet-orchestrator/src/orch/tick.rs"),
            ),
            (
                "orch/mod.rs",
                include_str!("../../../fleet-orchestrator/src/orch/mod.rs"),
            ),
            (
                "orch/session_context.rs",
                include_str!("../../../fleet-orchestrator/src/orch/session_context.rs"),
            ),
            (
                "store/tick_ops.rs",
                include_str!("../../../fleet-orchestrator/src/store/tick_ops.rs"),
            ),
            (
                "sandbox/team.rs",
                include_str!("../../../fleet-orchestrator/src/sandbox/team.rs"),
            ),
            (
                "fleet-host/spawn.rs",
                include_str!("../../../fleet-host/src/spawn.rs"),
            ),
            (
                "sandbox/worker_script.rs",
                include_str!("../../../fleet-orchestrator/src/sandbox/worker_script.rs"),
            ),
        ] {
            assert_spawn_does_not_read_workspace_role_prompt(label, src);
        }

        let cargo = include_str!("../../Cargo.toml");
        for dep in [
            "fleet-orchestrator",
            "async-openai",
            "async_openai",
            "async-anthropic",
            "ollama-rs",
            "xai-sdk",
        ] {
            assert!(
                !cargo.contains(dep),
                "fleet-auth must not depend on an LLM crate ({dep}); \
                 Agents & personas is persist-only (docs §6.7):\n{cargo}"
            );
        }

        let state = include_str!("../state.rs");
        assert!(
            state.contains("pub db: PgPool") && state.contains("pub config: AuthConfig"),
            "AuthState must stay db+config only (no LLM client):\n{state}"
        );
        assert_no_outbound_llm_client(state, "AuthState");

        let routes = production_src();
        assert_no_outbound_llm_client(routes, "fleet-auth personas.rs");

        assert!(
            !routes.contains("fleet_orchestrator")
                && !routes.contains("agent_loop")
                && !routes.contains("complete_text")
                && !routes.contains("ChatState")
                && !routes.contains("Grok")
                && !routes.contains("Ollama"),
            "persona routes must not reach the orchestrator LLM loop"
        );

        for (sig, repo) in [
            ("async fn list_personas", "list_for_workspace"),
            ("async fn get_persona", "persona_repo::get"),
            ("async fn create_persona", "persona_repo::insert"),
            ("async fn patch_persona", "persona_repo::update"),
            ("async fn delete_persona", "persona_repo::archive"),
            ("async fn seed_starters", "insert_if_absent"),
        ] {
            let body = fn_src(routes, sig);
            assert_no_outbound_llm_client(body, sig);
            assert!(
                body.contains(repo),
                "{sig} must persist via {repo} only (no LLM):\n{body}"
            );
        }

        // Docs §6.7 / §17: prompt/model/budget/tools are stored only.
        let create = fn_src(routes, "async fn create_persona");
        assert!(
            create.contains("body.role_prompt"),
            "create must persist the request role_prompt, not generate one:\n{create}"
        );
        let patch = fn_src(routes, "async fn patch_persona");
        assert!(
            patch.contains("body.role_prompt"),
            "PATCH must persist the request role_prompt, not generate one:\n{patch}"
        );
        let seed = fn_src(routes, "async fn seed_starters");
        assert!(
            seed.contains("s.role_prompt"),
            "seed must persist the catalog role_prompt, not generate one:\n{seed}"
        );

        let page = include_str!("../../../fleet-ui/src/pages/agents/page.rs");
        let page_prod = match page.find("#[cfg(test)]") {
            Some(i) => &page[..i],
            None => page,
        };
        assert_no_llm_call(page_prod, "agents/page.rs");
        assert!(
            page_prod.contains("future runs") && page_prod.contains("No LLM is called here"),
            "Agents page copy must say personas configure future runs and no LLM is called here (docs §6.7)"
        );
        for needed in [
            "seed_starters",
            "create_workspace_persona",
            "archive_workspace_persona",
            "list_workspace_personas",
        ] {
            assert!(
                page_prod.contains(needed),
                "Agents page must keep {needed} (seed/create/archive only)"
            );
        }
        assert!(
            !page_prod.contains("send_chat")
                && !page_prod.contains("chat_complete")
                && !page_prod.contains("/api/chat")
                && !page_prod.contains("generate_prompt")
                && !page_prod.contains("rewrite_prompt")
                && !page_prod.contains("suggest_prompt")
                && !page_prod.contains("launch_agent_plan("),
            "Agents page must not POST chat / complete an LLM turn"
        );
        assert!(
            !page_prod.contains("pages::chat") && !page_prod.contains("crate::pages::chat"),
            "Agents page must not import the chat client"
        );

        let api = include_str!("../../../fleet-ui/src/pages/agents/api.rs");
        let api_prod = match api.find("#[cfg(test)]") {
            Some(i) => &api[..i],
            None => api,
        };
        assert_no_llm_call(api_prod, "agents/api.rs");
        assert!(
            api_prod.contains("/personas")
                && api_prod.contains("/personas/seed-starters")
                && api_prod.contains("/personas/{persona_id}"),
            "Agents client must talk only to workspace personas CRUD"
        );
        assert!(
            !api_prod.contains("/chat")
                && !api_prod.contains("completions")
                && !api_prod.contains("/v1/")
                && !api_prod.contains("generate_prompt")
                && !api_prod.contains("rewrite_prompt"),
            "Agents client must not hit chat/completions"
        );
        // Control-plane reqwest is the cookie client. It must not grow a
        // generate/rewrite path next to seed/create/archive.
        assert!(
            api_prod.contains("list_workspace_personas")
                && api_prod.contains("seed_starters")
                && api_prod.contains("create_workspace_persona")
                && api_prod.contains("archive_workspace_persona"),
            "Agents client must keep seed/create/archive only"
        );
        for line in api_prod.lines() {
            let t = line.trim();
            if t.contains("/api/") {
                assert!(
                    t.contains("/personas"),
                    "Agents client must not grow a non-persona API path: {t}"
                );
            }
        }

        // Page load also GETs /fleet/personas (roster slugs). That must stay
        // a file/git list — not a chat completion.
        let fleet_routes = include_str!("../../../fleet-server/src/fleet_routes.rs");
        let get_roster = fn_src(fleet_routes, "async fn get_personas");
        assert_no_llm_call(get_roster, "fleet_routes get_personas");
        assert!(
            get_roster.contains("list_personas"),
            "GET /fleet/personas must list the roster, not complete a prompt:\n{get_roster}"
        );
        let roster = include_str!("../../../fleet-orchestrator/src/personas.rs");
        assert_no_llm_call(roster, "orchestrator personas.rs");
    }
}
