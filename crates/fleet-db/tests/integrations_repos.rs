//! Disconnect drops credentials; credentials_for fail-closed for the proxy.
//! Other workspaces' same provider stay intact.
//! Requires DATABASE_URL — never skips.

mod common;

use chrono::Utc;
use fleet_db::repos::integrations::{self, UpsertConnection};
use fleet_db::repos::users::NewUser;
use fleet_db::repos::{users, workspaces};
use serde_json::json;
use uuid::Uuid;

use common::{connect_and_migrate, unique_email, unique_slug};

async fn seed_user(pool: &fleet_db::PgPool, email: &str) -> Uuid {
    let id = Uuid::now_v7();
    let now = Utc::now();
    let mut tx = pool.begin().await.expect("begin");
    let user = users::insert_tx(
        &mut tx,
        &NewUser {
            id,
            email: email.to_string(),
            email_normalized: email.to_lowercase(),
            display_name: None,
            password_hash: "test-hash".into(),
            created_at: now,
        },
    )
    .await
    .expect("insert user");
    tx.commit().await.expect("commit user");
    assert_eq!(user.id, id);
    id
}

async fn seed_user_with_workspace(pool: &fleet_db::PgPool) -> (Uuid, Uuid) {
    let email = unique_email();
    let user_id = seed_user(pool, &email).await;
    let workspace_id = Uuid::now_v7();
    let slug = unique_slug("ws");
    let now = Utc::now();
    workspaces::create_owned(pool, workspace_id, user_id, &slug, "Integrations WS", now)
        .await
        .expect("create workspace");
    (user_id, workspace_id)
}

/// Ciphertext-shaped envelope (as stored after AES-256-GCM seal).
fn leftover_ciphertext(tag: &str) -> serde_json::Value {
    json!({
        "_enc": "v1",
        "nonce": format!("{tag}00112233445566778899aa"),
        "ct": format!("{tag}deadbeefcafebabe"),
    })
}

async fn upsert_github(
    pool: &fleet_db::PgPool,
    workspace_id: Uuid,
    user_id: Uuid,
    credentials: serde_json::Value,
) {
    integrations::upsert(
        pool,
        &UpsertConnection {
            id: Uuid::now_v7(),
            workspace_id,
            provider: "github".into(),
            account_label: Some("acct".into()),
            config: json!({ "site": "github.com" }),
            credentials,
            connected_by: Some(user_id),
            at: Utc::now(),
        },
    )
    .await
    .expect("upsert github");
}

#[tokio::test]
async fn disconnect_drops_credentials_other_workspace_same_provider_intact() {
    let pool = connect_and_migrate().await;
    let (user_a, ws_a) = seed_user_with_workspace(&pool).await;
    let (user_b, ws_b) = seed_user_with_workspace(&pool).await;

    upsert_github(&pool, ws_a, user_a, leftover_ciphertext("aaaa")).await;
    let leftover_b = leftover_ciphertext("bbbb");
    upsert_github(&pool, ws_b, user_b, leftover_b.clone()).await;

    let dropped = integrations::disconnect(&pool, ws_a, "github")
        .await
        .expect("disconnect ws-a");
    assert!(dropped, "connected row must disconnect");

    let after = integrations::credentials_for(&pool, ws_a, "github")
        .await
        .expect("creds a after");
    assert!(
        after.is_none(),
        "credentials_for must fail closed after disconnect, got {after:?}"
    );

    let all = integrations::list_all_for_workspace(&pool, ws_a)
        .await
        .expect("list_all");
    let row = all
        .iter()
        .find(|c| c.provider == "github")
        .expect("disconnected row remains");
    assert_eq!(row.status, "disconnected");
    assert_eq!(row.credentials, json!({}));

    let still = integrations::credentials_for(&pool, ws_b, "github")
        .await
        .expect("creds b after")
        .expect("ws-b still connected");
    assert_eq!(still, leftover_b);
}

#[tokio::test]
async fn disconnect_one_provider_leaves_other_provider_same_workspace_intact() {
    let pool = connect_and_migrate().await;
    let (user_a, ws_a) = seed_user_with_workspace(&pool).await;

    let leftover_gitlab = leftover_ciphertext("gl");
    upsert_github(&pool, ws_a, user_a, leftover_ciphertext("gh")).await;
    integrations::upsert(
        &pool,
        &UpsertConnection {
            id: Uuid::now_v7(),
            workspace_id: ws_a,
            provider: "gitlab".into(),
            account_label: Some("acct".into()),
            config: json!({ "site": "gitlab.com" }),
            credentials: leftover_gitlab.clone(),
            connected_by: Some(user_a),
            at: Utc::now(),
        },
    )
    .await
    .expect("upsert gitlab");

    let dropped = integrations::disconnect(&pool, ws_a, "github")
        .await
        .expect("disconnect github");
    assert!(dropped, "connected github must disconnect");

    let gitlab = integrations::credentials_for(&pool, ws_a, "gitlab")
        .await
        .expect("creds gitlab after")
        .expect("gitlab still connected");
    assert_eq!(gitlab, leftover_gitlab);
}

#[tokio::test]
async fn upsert_after_disconnect_reconnects_with_new_envelope() {
    let pool = connect_and_migrate().await;
    let (user_a, ws_a) = seed_user_with_workspace(&pool).await;

    upsert_github(&pool, ws_a, user_a, leftover_ciphertext("old")).await;
    let dropped = integrations::disconnect(&pool, ws_a, "github")
        .await
        .expect("disconnect");
    assert!(dropped, "connected github must disconnect");

    let leftover_new = leftover_ciphertext("new");
    upsert_github(&pool, ws_a, user_a, leftover_new.clone()).await;

    let row = integrations::find_connected(&pool, ws_a, "github")
        .await
        .expect("find_connected")
        .expect("reconnected");
    assert_eq!(row.status, "connected");
    assert_eq!(
        integrations::credentials_for(&pool, ws_a, "github")
            .await
            .expect("creds after upsert")
            .expect("connected envelope"),
        leftover_new
    );
}

#[tokio::test]
async fn credentials_for_returns_none_for_error_status() {
    let pool = connect_and_migrate().await;
    let (user_a, ws_a) = seed_user_with_workspace(&pool).await;

    upsert_github(&pool, ws_a, user_a, leftover_ciphertext("err")).await;
    integrations::mark_error(&pool, ws_a, "github", "token revoked")
        .await
        .expect("mark_error");

    assert!(
        integrations::credentials_for(&pool, ws_a, "github")
            .await
            .expect("creds after error")
            .is_none(),
        "credentials_for must fail closed for status=error"
    );
    assert!(
        integrations::find_connected(&pool, ws_a, "github")
            .await
            .expect("find_connected after error")
            .is_none(),
        "find_connected must not see an error row"
    );
    assert!(
        integrations::find_proxy_connection(&pool, ws_a, "github")
            .await
            .expect("proxy lookup")
            .is_none(),
        "an error row with no installation id must stay disconnected"
    );
}

#[tokio::test]
async fn error_row_with_installation_id_is_usable_until_cleared() {
    let pool = connect_and_migrate().await;
    let (user_a, ws_a) = seed_user_with_workspace(&pool).await;
    upsert_github(&pool, ws_a, user_a, leftover_ciphertext("inst")).await;
    sqlx::query(
        "UPDATE integration_connections SET config = '{\"installation_id\": 161434524}'::jsonb WHERE workspace_id = $1 AND provider = 'github'",
    )
    .bind(ws_a)
    .execute(&pool)
    .await
    .expect("set installation_id");
    integrations::mark_error(
        &pool,
        ws_a,
        "github",
        "GitHub API 403: Resource not accessible by integration",
    )
    .await
    .expect("mark_error");

    let row = integrations::find_proxy_connection(&pool, ws_a, "github")
        .await
        .expect("proxy lookup")
        .expect("installation error row stays callable");
    assert_eq!(row.status, "error");
    assert!(integrations::find_connected(&pool, ws_a, "github")
        .await
        .expect("find_connected")
        .is_none());

    assert!(integrations::clear_error(&pool, ws_a, "github")
        .await
        .expect("clear_error"));
    let connected = integrations::find_connected(&pool, ws_a, "github")
        .await
        .expect("find_connected after clear")
        .expect("cleared row is connected");
    assert!(connected.last_error.is_none());
}

#[tokio::test]
async fn list_for_workspace_includes_github_error_with_installation_id() {
    let pool = connect_and_migrate().await;
    let (user_a, ws_a) = seed_user_with_workspace(&pool).await;
    upsert_github(&pool, ws_a, user_a, leftover_ciphertext("list")).await;
    sqlx::query(
        "UPDATE integration_connections SET config = '{\"installation_id\": 42}'::jsonb WHERE workspace_id = $1 AND provider = 'github'",
    )
    .bind(ws_a)
    .execute(&pool)
    .await
    .expect("set installation_id");
    integrations::mark_error(&pool, ws_a, "github", "GitHub API 401: bad credentials")
        .await
        .expect("mark github");

    integrations::upsert(
        &pool,
        &UpsertConnection {
            id: Uuid::now_v7(),
            workspace_id: ws_a,
            provider: "slack".into(),
            account_label: Some("slack".into()),
            config: json!({}),
            credentials: leftover_ciphertext("slk"),
            connected_by: Some(user_a),
            at: Utc::now(),
        },
    )
    .await
    .expect("upsert slack");
    integrations::mark_error(&pool, ws_a, "slack", "invalid_auth")
        .await
        .expect("mark slack");

    let listed = integrations::list_for_workspace(&pool, ws_a)
        .await
        .expect("list");
    let providers: Vec<&str> = listed.iter().map(|r| r.provider.as_str()).collect();
    assert_eq!(providers, vec!["github", "slack"]);
    assert_eq!(listed[0].status, "error");
    assert_eq!(
        listed[0].last_error.as_deref(),
        Some("GitHub API 401: bad credentials")
    );
    assert_eq!(listed[1].status, "error");
    assert_eq!(listed[1].last_error.as_deref(), Some("invalid_auth"));
}

/// Global nonce PK: first insert wins for every workspace and provider.
#[tokio::test]
async fn consume_oauth_state_nonce_is_once_across_workspace_and_provider() {
    let pool = connect_and_migrate().await;
    let (_user_a, ws_a) = seed_user_with_workspace(&pool).await;
    let (_user_b, ws_b) = seed_user_with_workspace(&pool).await;
    let nonce = format!("oauth-nonce-{}", Uuid::now_v7());

    let first = integrations::consume_oauth_state_nonce(&pool, &nonce, ws_a, "github")
        .await
        .expect("first consume");
    assert!(first, "first insert must win");

    let cross = integrations::consume_oauth_state_nonce(&pool, &nonce, ws_b, "gitlab")
        .await
        .expect("cross workspace/provider consume");
    assert!(
        !cross,
        "same nonce must stay dead across workspace and provider"
    );
}

/// Trim-empty is not a consumable nonce; insert never runs.
#[tokio::test]
async fn consume_oauth_state_nonce_rejects_blank_or_whitespace() {
    let pool = connect_and_migrate().await;
    let (_user_a, ws_a) = seed_user_with_workspace(&pool).await;

    let empty = integrations::consume_oauth_state_nonce(&pool, "", ws_a, "github")
        .await
        .expect("empty nonce");
    let whitespace = integrations::consume_oauth_state_nonce(&pool, "   \t", ws_a, "github")
        .await
        .expect("whitespace nonce");
    assert!(!empty, "empty nonce must not consume");
    assert!(!whitespace, "whitespace nonce must not consume");
}

/// Workspace delete CASCADE drops the nonce PK so a new workspace can win it.
#[tokio::test]
async fn consume_oauth_state_nonce_wins_again_after_workspace_delete() {
    let pool = connect_and_migrate().await;
    let (_user_c, ws_c) = seed_user_with_workspace(&pool).await;
    let (_user_d, ws_d) = seed_user_with_workspace(&pool).await;
    let nonce = format!("oauth-nonce-cascade-{}", Uuid::now_v7());

    let first = integrations::consume_oauth_state_nonce(&pool, &nonce, ws_c, "github")
        .await
        .expect("consume before delete");
    assert!(first, "first insert must win");

    sqlx::query("DELETE FROM workspace_members WHERE workspace_id = $1")
        .bind(ws_c)
        .execute(&pool)
        .await
        .expect("delete members");
    sqlx::query("DELETE FROM workspaces WHERE id = $1")
        .bind(ws_c)
        .execute(&pool)
        .await
        .expect("delete workspace");

    let again = integrations::consume_oauth_state_nonce(&pool, &nonce, ws_d, "github")
        .await
        .expect("consume after workspace delete");
    assert!(
        again,
        "workspace ON DELETE CASCADE must free the nonce for a new workspace"
    );
}

/// PATCH defaults UPDATE a connected row only: three keys, leftover config,
/// credentials/status stay put, version bumps. Missing provider is NotFound.
#[tokio::test]
async fn patch_agent_defaults_writes_keys_on_connected_and_leaves_credentials() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let credentials = leftover_ciphertext("patch");

    integrations::upsert(
        &pool,
        &UpsertConnection {
            id: Uuid::now_v7(),
            workspace_id,
            provider: "github".into(),
            account_label: Some("acct".into()),
            config: json!({ "site": "github.com", "extra": "keep-me" }),
            credentials: credentials.clone(),
            connected_by: Some(user_id),
            at: Utc::now(),
        },
    )
    .await
    .expect("upsert github");

    let before = integrations::find_connected(&pool, workspace_id, "github")
        .await
        .expect("find before")
        .expect("connected");
    assert_eq!(before.status, "connected");
    assert_eq!(before.credentials, credentials);

    let patched = integrations::patch_agent_defaults(
        &pool,
        workspace_id,
        "github",
        Some("runner-1"),
        Some("grok"),
        Some("model-1"),
    )
    .await
    .expect("patch defaults");

    assert_eq!(patched.status, "connected");
    assert_eq!(patched.credentials, credentials);
    assert_eq!(patched.version, before.version + 1);
    assert_eq!(patched.config["site"], "github.com");
    assert_eq!(patched.config["extra"], "keep-me");
    assert_eq!(patched.config["default_runner_id"], "runner-1");
    assert_eq!(patched.config["default_provider"], "grok");
    assert_eq!(patched.config["default_model"], "model-1");

    let missing = integrations::patch_agent_defaults(
        &pool,
        workspace_id,
        "gitlab",
        Some("runner-1"),
        Some("grok"),
        Some("model-1"),
    )
    .await
    .expect_err("missing provider must not insert");
    match missing {
        fleet_db::DbError::NotFound(msg) => {
            assert!(
                msg.to_ascii_lowercase().contains("not connected"),
                "expected not-connected NotFound, got {msg}"
            );
        }
        other => panic!("expected NotFound, got {other:?}"),
    }

    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*)::bigint FROM integration_connections WHERE workspace_id = $1",
    )
    .bind(workspace_id)
    .fetch_one(&pool)
    .await
    .expect("count integration_connections");
    assert_eq!(count, 1, "patch must not INSERT a connections row");
}
