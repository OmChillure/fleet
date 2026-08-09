//! Workspace personas — agent role configs (no LLM calls).

use axum::Json;
use axum::extract::{FromRef, Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Router;
use axum_extra::extract::CookieJar;
use chrono::Utc;
use fleet_db::repos::{personas as persona_repo, workspaces as workspace_repo};
use fleet_db::{PersonaPublic, repos::personas::{NewPersona, UpdatePersona}};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
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
            get(get_persona)
                .patch(patch_persona)
                .delete(delete_persona),
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
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::bad_request("name is required"));
    }
    if name.len() > 80 {
        return Err(AppError::bad_request("name is too long"));
    }
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
        description: body.description.unwrap_or_default().trim().to_string(),
        icon: body
            .icon
            .unwrap_or_else(|| "bot".into())
            .trim()
            .to_string(),
        role_prompt: body.role_prompt.unwrap_or_default(),
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
    let patch = UpdatePersona {
        name: body.name.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        description: body.description,
        icon: body.icon,
        role_prompt: body.role_prompt,
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
}
