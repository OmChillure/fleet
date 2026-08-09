use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Full persona row.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct Persona {
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
    pub status: String,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub version: i64,
}

/// Public API shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaPublic {
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
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<Persona> for PersonaPublic {
    fn from(p: Persona) -> Self {
        Self {
            id: p.id,
            workspace_id: p.workspace_id,
            slug: p.slug,
            name: p.name,
            description: p.description,
            icon: p.icon,
            role_prompt: p.role_prompt,
            worker_profile: p.worker_profile,
            model_preference: p.model_preference,
            temperature: p.temperature,
            max_budget_usd: p.max_budget_usd,
            tools: p.tools,
            is_starter: p.is_starter,
            is_default: p.is_default,
            status: p.status,
            created_at: p.created_at,
            updated_at: p.updated_at,
        }
    }
}
