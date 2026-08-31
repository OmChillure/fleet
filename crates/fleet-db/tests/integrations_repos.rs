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

    let leftover_a = leftover_ciphertext("aaaa");
    let leftover_b = leftover_ciphertext("bbbb");
    upsert_github(&pool, ws_a, user_a, leftover_a.clone()).await;
    upsert_github(&pool, ws_b, user_b, leftover_b.clone()).await;

    assert_eq!(
        integrations::credentials_for(&pool, ws_a, "github")
            .await
            .expect("creds a before")
            .expect("ws-a connected"),
        leftover_a
    );
    assert_eq!(
        integrations::credentials_for(&pool, ws_b, "github")
            .await
            .expect("creds b before")
            .expect("ws-b connected"),
        leftover_b
    );

    let dropped = integrations::disconnect(&pool, ws_a, "github")
        .await
        .expect("disconnect ws-a");
    assert!(dropped, "connected row must disconnect");

    // Proxy path: credentials_for uses find_connected — must not return leftover ct.
    let after = integrations::credentials_for(&pool, ws_a, "github")
        .await
        .expect("creds a after");
    assert!(
        after.is_none(),
        "credentials_for must fail closed after disconnect, got {after:?}"
    );
    assert!(
        integrations::find_connected(&pool, ws_a, "github")
            .await
            .expect("find_connected a")
            .is_none(),
        "find_connected must not see a disconnected row"
    );

    // Soft-delete row remains; secrets must be wiped (not leftover envelope).
    let all = integrations::list_all_for_workspace(&pool, ws_a)
        .await
        .expect("list_all");
    let row = all
        .iter()
        .find(|c| c.provider == "github")
        .expect("disconnected row remains");
    assert_eq!(row.status, "disconnected");
    assert_eq!(row.credentials, json!({}));
    assert!(row.credentials.get("_enc").is_none());
    assert!(row.credentials.get("ct").is_none());
    assert!(row.credentials.get("access_token").is_none());
    assert!(row.last_error.is_none());

    let listed = integrations::list_for_workspace(&pool, ws_a)
        .await
        .expect("list connected");
    assert!(
        listed.iter().all(|c| c.provider != "github"),
        "list_for_workspace must omit disconnected github"
    );

    // Other workspace, same provider: still connected, same ciphertext.
    let still = integrations::credentials_for(&pool, ws_b, "github")
        .await
        .expect("creds b after")
        .expect("ws-b still connected");
    assert_eq!(still, leftover_b);
    let b_row = integrations::find_connected(&pool, ws_b, "github")
        .await
        .expect("find_connected b")
        .expect("ws-b connected");
    assert_eq!(b_row.status, "connected");
    assert_eq!(b_row.credentials, leftover_b);

    // Already disconnected: no-op, still no leftover for the proxy.
    let again = integrations::disconnect(&pool, ws_a, "github")
        .await
        .expect("second disconnect");
    assert!(!again, "already-disconnected must not report a drop");
    assert!(integrations::credentials_for(&pool, ws_a, "github")
        .await
        .expect("creds a after second")
        .is_none());
    let still_b = integrations::credentials_for(&pool, ws_b, "github")
        .await
        .expect("creds b after second")
        .expect("ws-b still connected after second disconnect");
    assert_eq!(still_b, leftover_b);
}

async fn count_oauth_nonces(pool: &fleet_db::PgPool, nonce: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*)::bigint FROM integration_oauth_state_nonces WHERE nonce = $1",
    )
    .bind(nonce)
    .fetch_one(pool)
    .await
    .expect("count oauth nonce")
}

async fn count_workspace_oauth_nonces(pool: &fleet_db::PgPool, workspace_id: Uuid) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*)::bigint FROM integration_oauth_state_nonces WHERE workspace_id = $1",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .expect("count workspace oauth nonces")
}

/// Insert-on-conflict is the consume-once lock. Rows survive disconnect;
/// workspace delete cascades.
#[tokio::test]
async fn consume_oauth_state_nonce_is_once_across_workspace_and_survives_disconnect() {
    let pool = connect_and_migrate().await;
    let (user_a, ws_a) = seed_user_with_workspace(&pool).await;
    let (_user_b, ws_b) = seed_user_with_workspace(&pool).await;
    let nonce = format!("oauth-nonce-{}", Uuid::now_v7());

    let first = integrations::consume_oauth_state_nonce(&pool, &nonce, ws_a, "github")
        .await
        .expect("first consume");
    assert!(first, "first insert must win");
    assert_eq!(count_oauth_nonces(&pool, &nonce).await, 1);

    let replay = integrations::consume_oauth_state_nonce(&pool, &nonce, ws_a, "github")
        .await
        .expect("replay consume");
    assert!(!replay, "replay of the same nonce must lose");
    assert_eq!(count_oauth_nonces(&pool, &nonce).await, 1);

    let cross = integrations::consume_oauth_state_nonce(&pool, &nonce, ws_b, "gitlab")
        .await
        .expect("cross workspace/provider consume");
    assert!(
        !cross,
        "same nonce must stay dead across workspace and provider"
    );
    assert_eq!(count_oauth_nonces(&pool, &nonce).await, 1);

    let empty = integrations::consume_oauth_state_nonce(&pool, "", ws_a, "github")
        .await
        .expect("empty nonce");
    let whitespace = integrations::consume_oauth_state_nonce(&pool, "   \t", ws_a, "github")
        .await
        .expect("whitespace nonce");
    assert!(!empty, "empty nonce must not consume");
    assert!(!whitespace, "whitespace nonce must not consume");
    assert_eq!(
        count_oauth_nonces(&pool, "").await,
        0,
        "empty nonce must not insert a row"
    );
    assert_eq!(
        count_oauth_nonces(&pool, "   \t").await,
        0,
        "whitespace nonce must not insert a row"
    );
    assert_eq!(
        count_workspace_oauth_nonces(&pool, ws_a).await,
        1,
        "empty/whitespace must not add rows for the workspace"
    );

    upsert_github(&pool, ws_a, user_a, leftover_ciphertext("nonce")).await;
    let dropped = integrations::disconnect(&pool, ws_a, "github")
        .await
        .expect("disconnect");
    assert!(dropped, "connected github must disconnect");
    assert_eq!(
        count_oauth_nonces(&pool, &nonce).await,
        1,
        "disconnect must not delete the nonce row"
    );
    let after_disconnect = integrations::consume_oauth_state_nonce(&pool, &nonce, ws_a, "github")
        .await
        .expect("consume after disconnect");
    assert!(
        !after_disconnect,
        "nonce must stay consumed after disconnect"
    );

    let (_user_c, ws_c) = seed_user_with_workspace(&pool).await;
    let cascade_nonce = format!("oauth-nonce-cascade-{}", Uuid::now_v7());
    let cascaded = integrations::consume_oauth_state_nonce(&pool, &cascade_nonce, ws_c, "github")
        .await
        .expect("cascade nonce");
    assert!(cascaded);
    assert_eq!(count_oauth_nonces(&pool, &cascade_nonce).await, 1);

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
    assert_eq!(
        count_oauth_nonces(&pool, &cascade_nonce).await,
        0,
        "workspace ON DELETE CASCADE must drop the nonce row"
    );
    assert_eq!(
        count_oauth_nonces(&pool, &nonce).await,
        1,
        "other workspace nonces must stay"
    );
}
