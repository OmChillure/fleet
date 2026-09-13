//! Workspace chat conversations and messages.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::DbResult;
use crate::models::{ChatConversation, ChatConversationSummary, ChatMessage};
use crate::pool::map_query_err;

const CONV_COLS: &str = "id, workspace_id, user_id, title, provider, model, created_at, updated_at";
const MSG_COLS: &str = "id, conversation_id, role, content, provider, model, actions, created_at";

pub async fn create_conversation(
    pool: &PgPool,
    id: Uuid,
    workspace_id: Uuid,
    user_id: Uuid,
    title: &str,
    provider: Option<&str>,
    model: Option<&str>,
    at: DateTime<Utc>,
) -> DbResult<ChatConversation> {
    let q = format!(
        r#"
        INSERT INTO chat_conversations (
            id, workspace_id, user_id, title, provider, model, created_at, updated_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $7)
        RETURNING {CONV_COLS}
        "#
    );
    sqlx::query_as::<_, ChatConversation>(&q)
        .bind(id)
        .bind(workspace_id)
        .bind(user_id)
        .bind(title)
        .bind(provider)
        .bind(model)
        .bind(at)
        .fetch_one(pool)
        .await
        .map_err(map_query_err)
}

pub async fn get_conversation(
    pool: &PgPool,
    conversation_id: Uuid,
    workspace_id: Uuid,
    user_id: Uuid,
) -> DbResult<Option<ChatConversation>> {
    let q = format!(
        r#"
        SELECT {CONV_COLS}
        FROM chat_conversations
        WHERE id = $1 AND workspace_id = $2 AND user_id = $3
        "#
    );
    sqlx::query_as::<_, ChatConversation>(&q)
        .bind(conversation_id)
        .bind(workspace_id)
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

pub async fn list_conversations(
    pool: &PgPool,
    workspace_id: Uuid,
    user_id: Uuid,
    limit: i64,
) -> DbResult<Vec<ChatConversationSummary>> {
    let limit = limit.clamp(1, 200);
    let rows = sqlx::query_as::<_, ConversationListRow>(
        r#"
        SELECT
            c.id,
            c.workspace_id,
            c.title,
            c.provider,
            c.model,
            c.created_at,
            c.updated_at,
            COALESCE(mc.cnt, 0)::bigint AS message_count,
            prev.content AS preview
        FROM chat_conversations c
        LEFT JOIN LATERAL (
            SELECT COUNT(*)::bigint AS cnt
            FROM chat_messages m
            WHERE m.conversation_id = c.id
        ) mc ON true
        LEFT JOIN LATERAL (
            SELECT m.content
            FROM chat_messages m
            WHERE m.conversation_id = c.id
              AND m.role IN ('user', 'assistant')
            ORDER BY m.created_at DESC
            LIMIT 1
        ) prev ON true
        WHERE c.workspace_id = $1 AND c.user_id = $2
          AND EXISTS (
            SELECT 1 FROM chat_messages m WHERE m.conversation_id = c.id
          )
        ORDER BY c.updated_at DESC
        LIMIT $3
        "#,
    )
    .bind(workspace_id)
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_query_err)?;

    Ok(rows
        .into_iter()
        .map(|r| ChatConversationSummary {
            id: r.id,
            workspace_id: r.workspace_id,
            title: r.title,
            provider: r.provider,
            model: r.model,
            created_at: r.created_at,
            updated_at: r.updated_at,
            message_count: r.message_count,
            preview: r.preview.map(|s| {
                let t = s.trim();
                if t.chars().count() > 160 {
                    format!("{}…", t.chars().take(157).collect::<String>())
                } else {
                    t.to_string()
                }
            }),
        })
        .collect())
}

#[derive(Debug, sqlx::FromRow)]
struct ConversationListRow {
    id: Uuid,
    workspace_id: Uuid,
    title: String,
    provider: Option<String>,
    model: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    message_count: i64,
    preview: Option<String>,
}

pub async fn delete_conversation(
    pool: &PgPool,
    conversation_id: Uuid,
    workspace_id: Uuid,
    user_id: Uuid,
) -> DbResult<bool> {
    let res = sqlx::query(
        r#"
        DELETE FROM chat_conversations
        WHERE id = $1 AND workspace_id = $2 AND user_id = $3
        "#,
    )
    .bind(conversation_id)
    .bind(workspace_id)
    .bind(user_id)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(res.rows_affected() > 0)
}

/// Drop a thread that never stored a message (failed first turn).
pub async fn delete_empty_conversation(
    pool: &PgPool,
    conversation_id: Uuid,
    workspace_id: Uuid,
    user_id: Uuid,
) -> DbResult<bool> {
    let res = sqlx::query(
        r#"
        DELETE FROM chat_conversations c
        WHERE c.id = $1 AND c.workspace_id = $2 AND c.user_id = $3
          AND NOT EXISTS (
            SELECT 1 FROM chat_messages m WHERE m.conversation_id = c.id
          )
        "#,
    )
    .bind(conversation_id)
    .bind(workspace_id)
    .bind(user_id)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(res.rows_affected() > 0)
}

pub async fn touch_conversation(
    pool: &PgPool,
    conversation_id: Uuid,
    title: Option<&str>,
    provider: Option<&str>,
    model: Option<&str>,
    at: DateTime<Utc>,
) -> DbResult<()> {
    sqlx::query(
        r#"
        UPDATE chat_conversations
        SET
            updated_at = $2,
            title = COALESCE($3, title),
            provider = COALESCE($4, provider),
            model = COALESCE($5, model)
        WHERE id = $1
        "#,
    )
    .bind(conversation_id)
    .bind(at)
    .bind(title)
    .bind(provider)
    .bind(model)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(())
}

pub async fn list_messages(pool: &PgPool, conversation_id: Uuid) -> DbResult<Vec<ChatMessage>> {
    let q = format!(
        r#"
        SELECT {MSG_COLS}
        FROM chat_messages
        WHERE conversation_id = $1
        ORDER BY created_at ASC
        "#
    );
    sqlx::query_as::<_, ChatMessage>(&q)
        .bind(conversation_id)
        .fetch_all(pool)
        .await
        .map_err(map_query_err)
}

pub async fn insert_message(
    pool: &PgPool,
    id: Uuid,
    conversation_id: Uuid,
    role: &str,
    content: &str,
    provider: Option<&str>,
    model: Option<&str>,
    actions: &Value,
    at: DateTime<Utc>,
) -> DbResult<ChatMessage> {
    let q = format!(
        r#"
        INSERT INTO chat_messages (
            id, conversation_id, role, content, provider, model, actions, created_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        RETURNING {MSG_COLS}
        "#
    );
    sqlx::query_as::<_, ChatMessage>(&q)
        .bind(id)
        .bind(conversation_id)
        .bind(role)
        .bind(content)
        .bind(provider)
        .bind(model)
        .bind(actions)
        .bind(at)
        .fetch_one(pool)
        .await
        .map_err(map_query_err)
}

/// Last composer pick for this workspace (provider + model from the newest chat).
pub async fn latest_composer_llm(
    pool: &PgPool,
    workspace_id: Uuid,
) -> DbResult<Option<(String, String)>> {
    let row: Option<(Option<String>, Option<String>)> = sqlx::query_as(
        r#"
        SELECT provider, model
        FROM chat_conversations
        WHERE workspace_id = $1
          AND provider IS NOT NULL
          AND btrim(provider) <> ''
        ORDER BY updated_at DESC
        LIMIT 1
        "#,
    )
    .bind(workspace_id)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)?;
    Ok(row.and_then(|(p, m)| {
        let provider = p
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())?;
        let model = m.unwrap_or_default().trim().to_string();
        Some((provider, model))
    }))
}

pub async fn count_messages(pool: &PgPool, conversation_id: Uuid) -> DbResult<i64> {
    let n: (i64,) =
        sqlx::query_as(r#"SELECT COUNT(*)::bigint FROM chat_messages WHERE conversation_id = $1"#)
            .bind(conversation_id)
            .fetch_one(pool)
            .await
            .map_err(map_query_err)?;
    Ok(n.0)
}
