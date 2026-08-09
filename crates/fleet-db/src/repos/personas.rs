use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::DbResult;
use crate::models::Persona;
use crate::pool::map_query_err;

const COLS: &str = r#"
    id, workspace_id, slug, name, description, icon, role_prompt, worker_profile,
    model_preference, temperature, max_budget_usd, tools, is_starter, is_default,
    status, created_by, created_at, updated_at, version
"#;

#[derive(Debug, Clone)]
pub struct NewPersona {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub icon: String,
    pub role_prompt: String,
    pub worker_profile: String,
    pub model_preference: Option<String>,
    pub temperature: Option<f64>,
    pub max_budget_usd: Option<f64>,
    pub tools: Value,
    pub is_starter: bool,
    pub is_default: bool,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default)]
pub struct UpdatePersona {
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

pub async fn list_for_workspace(
    pool: &PgPool,
    workspace_id: Uuid,
    status: Option<&str>,
) -> DbResult<Vec<Persona>> {
    let status = status.unwrap_or("active");
    sqlx::query_as::<_, Persona>(&format!(
        r#"
        SELECT {COLS}
        FROM personas
        WHERE workspace_id = $1 AND status = $2
        ORDER BY is_default DESC, name ASC
        "#
    ))
    .bind(workspace_id)
    .bind(status)
    .fetch_all(pool)
    .await
    .map_err(map_query_err)
}

pub async fn get(
    pool: &PgPool,
    workspace_id: Uuid,
    persona_id: Uuid,
) -> DbResult<Option<Persona>> {
    sqlx::query_as::<_, Persona>(&format!(
        r#"
        SELECT {COLS}
        FROM personas
        WHERE workspace_id = $1 AND id = $2
        "#
    ))
    .bind(workspace_id)
    .bind(persona_id)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)
}

pub async fn insert(pool: &PgPool, new: &NewPersona) -> DbResult<Persona> {
    if new.is_default {
        clear_default(pool, new.workspace_id).await?;
    }
    sqlx::query_as::<_, Persona>(&format!(
        r#"
        INSERT INTO personas (
            id, workspace_id, slug, name, description, icon, role_prompt,
            worker_profile, model_preference, temperature, max_budget_usd, tools,
            is_starter, is_default, status, created_by, created_at, updated_at
        )
        VALUES (
            $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,'active',$15,$16,$16
        )
        RETURNING {COLS}
        "#
    ))
    .bind(new.id)
    .bind(new.workspace_id)
    .bind(&new.slug)
    .bind(&new.name)
    .bind(&new.description)
    .bind(&new.icon)
    .bind(&new.role_prompt)
    .bind(&new.worker_profile)
    .bind(&new.model_preference)
    .bind(new.temperature)
    .bind(new.max_budget_usd)
    .bind(&new.tools)
    .bind(new.is_starter)
    .bind(new.is_default)
    .bind(new.created_by)
    .bind(new.created_at)
    .fetch_one(pool)
    .await
    .map_err(map_query_err)
}

/// Insert if slug missing; returns whether a row was inserted.
pub async fn insert_if_absent(pool: &PgPool, new: &NewPersona) -> DbResult<bool> {
    let r = sqlx::query(
        r#"
        INSERT INTO personas (
            id, workspace_id, slug, name, description, icon, role_prompt,
            worker_profile, model_preference, temperature, max_budget_usd, tools,
            is_starter, is_default, status, created_by, created_at, updated_at
        )
        VALUES (
            $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,'active',$15,$16,$16
        )
        ON CONFLICT (workspace_id, slug) DO NOTHING
        "#,
    )
    .bind(new.id)
    .bind(new.workspace_id)
    .bind(&new.slug)
    .bind(&new.name)
    .bind(&new.description)
    .bind(&new.icon)
    .bind(&new.role_prompt)
    .bind(&new.worker_profile)
    .bind(&new.model_preference)
    .bind(new.temperature)
    .bind(new.max_budget_usd)
    .bind(&new.tools)
    .bind(new.is_starter)
    .bind(new.is_default)
    .bind(new.created_by)
    .bind(new.created_at)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(r.rows_affected() > 0)
}

pub async fn update(
    pool: &PgPool,
    workspace_id: Uuid,
    persona_id: Uuid,
    patch: &UpdatePersona,
    at: DateTime<Utc>,
) -> DbResult<Option<Persona>> {
    let current = match get(pool, workspace_id, persona_id).await? {
        Some(p) => p,
        None => return Ok(None),
    };
    if patch.is_default == Some(true) {
        clear_default(pool, workspace_id).await?;
    }

    let name = patch.name.as_ref().unwrap_or(&current.name);
    let description = patch.description.as_ref().unwrap_or(&current.description);
    let icon = patch.icon.as_ref().unwrap_or(&current.icon);
    let role_prompt = patch.role_prompt.as_ref().unwrap_or(&current.role_prompt);
    let worker_profile = patch
        .worker_profile
        .as_ref()
        .unwrap_or(&current.worker_profile);
    let model_preference = match &patch.model_preference {
        Some(v) => v.clone(),
        None => current.model_preference.clone(),
    };
    let temperature = match patch.temperature {
        Some(v) => v,
        None => current.temperature,
    };
    let max_budget_usd = match patch.max_budget_usd {
        Some(v) => v,
        None => current.max_budget_usd,
    };
    let tools = patch.tools.as_ref().unwrap_or(&current.tools);
    let is_default = patch.is_default.unwrap_or(current.is_default);
    let status = patch.status.as_ref().unwrap_or(&current.status);

    sqlx::query_as::<_, Persona>(&format!(
        r#"
        UPDATE personas SET
            name = $3,
            description = $4,
            icon = $5,
            role_prompt = $6,
            worker_profile = $7,
            model_preference = $8,
            temperature = $9,
            max_budget_usd = $10,
            tools = $11,
            is_default = $12,
            status = $13,
            updated_at = $14,
            version = version + 1
        WHERE workspace_id = $1 AND id = $2
        RETURNING {COLS}
        "#
    ))
    .bind(workspace_id)
    .bind(persona_id)
    .bind(name)
    .bind(description)
    .bind(icon)
    .bind(role_prompt)
    .bind(worker_profile)
    .bind(model_preference)
    .bind(temperature)
    .bind(max_budget_usd)
    .bind(tools)
    .bind(is_default)
    .bind(status)
    .bind(at)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)
}

pub async fn archive(
    pool: &PgPool,
    workspace_id: Uuid,
    persona_id: Uuid,
    at: DateTime<Utc>,
) -> DbResult<bool> {
    let r = sqlx::query(
        r#"
        UPDATE personas
        SET status = 'archived', is_default = false, updated_at = $3, version = version + 1
        WHERE workspace_id = $1 AND id = $2 AND status = 'active'
        "#,
    )
    .bind(workspace_id)
    .bind(persona_id)
    .bind(at)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(r.rows_affected() > 0)
}

async fn clear_default(pool: &PgPool, workspace_id: Uuid) -> DbResult<()> {
    sqlx::query(
        r#"
        UPDATE personas SET is_default = false, updated_at = now()
        WHERE workspace_id = $1 AND is_default = true
        "#,
    )
    .bind(workspace_id)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(())
}
