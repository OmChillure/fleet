//! Workspace B must not invoke via workspace A's integration connection.
//! Requires DATABASE_URL — never skips.

use std::time::Duration;

use chrono::Utc;
use fleet_db::repos::integrations::{self, UpsertConnection};
use fleet_db::repos::users::NewUser;
use fleet_db::repos::{users, workspaces};
use fleet_integrations::proxy::{ProxyCaller, ProxyRequest};
use fleet_integrations::{CredentialsKey, proxy};
use serde_json::{Value, json};
use uuid::Uuid;

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

async fn connect_and_migrate() -> fleet_db::PgPool {
    let url = require_database_url();
    fleet_db::connect_and_migrate_with(
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
    .unwrap_or_else(|e| panic!("connect_and_migrate failed: {e}"))
}

async fn seed_user_with_workspace(pool: &fleet_db::PgPool, tag: &str) -> (Uuid, Uuid) {
    let user_id = Uuid::now_v7();
    let email = format!("proxy.iso.{tag}.{user_id}@example.test");
    let now = Utc::now();
    let mut tx = pool.begin().await.expect("begin user");
    users::insert_tx(
        &mut tx,
        &NewUser {
            id: user_id,
            email: email.clone(),
            email_normalized: email.to_lowercase(),
            display_name: None,
            password_hash: "test-hash".into(),
            created_at: now,
        },
    )
    .await
    .expect("insert user");
    tx.commit().await.expect("commit user");

    let workspace_id = Uuid::now_v7();
    let slug = format!("pxy-{tag}-{}", &workspace_id.to_string()[..8]);
    workspaces::create_owned(
        pool,
        workspace_id,
        user_id,
        &slug,
        "Proxy isolation WS",
        now,
    )
    .await
    .expect("create workspace");
    (user_id, workspace_id)
}

/// Product path: `proxy::invoke` as ws-B must not load ws-A's sealed github token
/// even when args name that connection / workspace.
#[tokio::test]
async fn invoke_workspace_b_cannot_use_workspace_a_connection() {
    let pool = connect_and_migrate().await;
    let (user_a, ws_a) = seed_user_with_workspace(&pool, "a").await;
    let (user_b, ws_b) = seed_user_with_workspace(&pool, "b").await;
    let conn_a = Uuid::now_v7();
    let token = format!("tok_ws_a_must_not_leak_{}", Uuid::new_v4());
    let key = CredentialsKey::from_secret("dev-test-encryption-key-32chars!!").expect("test key");
    let sealed = key
        .encrypt(&json!({ "access_token": token }))
        .expect("seal ws-A token");

    integrations::upsert(
        &pool,
        &UpsertConnection {
            id: conn_a,
            workspace_id: ws_a,
            provider: "github".into(),
            account_label: Some("acct-a".into()),
            config: json!({ "site": "github.com" }),
            credentials: sealed.clone(),
            connected_by: Some(user_a),
            at: Utc::now(),
        },
    )
    .await
    .expect("upsert github on ws-A");

    let before = integrations::find_connected(&pool, ws_a, "github")
        .await
        .expect("find_connected a before")
        .expect("ws-A github connected");
    assert_eq!(before.id, conn_a);
    assert_eq!(before.workspace_id, ws_a);

    let result = proxy::invoke(
        &pool,
        &key,
        ProxyRequest {
            workspace_id: ws_b,
            provider: "github".into(),
            operation: "list_prs".into(),
            args: json!({
                "connection_id": conn_a,
                "workspace_id": ws_a,
                "id": conn_a,
            }),
            caller: ProxyCaller::Chat { user_id: user_b },
        },
    )
    .await;

    assert!(
        !result.ok,
        "foreign connection must fail closed: {result:?}"
    );
    assert_eq!(
        result.error_code.as_deref(),
        Some("not_connected"),
        "ws-B has no github row; args must not retarget to ws-A: {result:?}"
    );
    assert_eq!(result.data, Value::Null);
    let dump = serde_json::to_string(&result).expect("serialize result");
    assert!(
        !dump.contains(&token),
        "proxy result must not leak ws-A token: {dump}"
    );
    assert!(
        !result.summary.contains(&token),
        "summary must not leak ws-A token: {}",
        result.summary
    );

    let after = integrations::find_connected(&pool, ws_a, "github")
        .await
        .expect("find_connected a after")
        .expect("ws-A github row must stay connected");
    assert_eq!(after.id, conn_a);
    assert_eq!(after.workspace_id, ws_a);
    assert_eq!(after.status, "connected");
    assert_eq!(after.credentials, sealed);

    assert!(
        integrations::find_connected(&pool, ws_b, "github")
            .await
            .expect("find_connected b")
            .is_none(),
        "ws-B must still have no github row"
    );
}

/// Docs §7.3: live `proxy::invoke` decrypts on the control plane and never
/// echoes the token. Not a reconstructed `ProxyResult` / `sanitize_value` lock.
#[tokio::test]
async fn integration_query_decrypts_server_side_and_never_echoes_token() {
    const LEAK: &str = "gho_IQ_LIVE_SANITIZE_MUST_NOT_ECHO_c0ffee";

    fn assert_no_token_echo(label: &str, dump: &str) {
        assert!(
            !dump.contains("access_token")
                && !dump.contains("refresh_token")
                && !dump.contains(LEAK),
            "{label} must not contain access_token, refresh_token, or the sealed token: {dump}"
        );
    }

    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool, "iqdec").await;
    assert!(
        integrations::find_connected(&pool, workspace_id, "github")
            .await
            .expect("find_connected empty")
            .is_none(),
        "workspace must start with no github row"
    );

    let stuffed = json!({
        "access_token": LEAK,
        "refresh_token": LEAK,
        "token": LEAK,
        "owner": "acme",
        "repo": "app",
    });
    let key_a = CredentialsKey::from_secret("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").expect("key A");
    let key_b = CredentialsKey::from_secret("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").expect("key B");

    let unknown = proxy::invoke(
        &pool,
        &key_b,
        ProxyRequest {
            workspace_id,
            provider: "github".into(),
            operation: "raw_http".into(),
            args: stuffed.clone(),
            caller: ProxyCaller::Chat { user_id },
        },
    )
    .await;
    assert!(!unknown.ok, "unknown op must be ok:false: {unknown:?}");
    assert_eq!(
        unknown.error_code.as_deref(),
        Some("unknown_op"),
        "docs §7.3 unknown op must be unknown_op: {unknown:?}"
    );
    assert_eq!(unknown.data, Value::Null);
    assert_no_token_echo(
        "unknown_op ProxyResult",
        &serde_json::to_string(&unknown).expect("serialize unknown"),
    );

    let disconnected = proxy::invoke(
        &pool,
        &key_b,
        ProxyRequest {
            workspace_id,
            provider: "github".into(),
            operation: "list_repos".into(),
            args: stuffed.clone(),
            caller: ProxyCaller::Chat { user_id },
        },
    )
    .await;
    assert!(
        !disconnected.ok,
        "disconnected list_repos must be ok:false: {disconnected:?}"
    );
    assert_eq!(
        disconnected.error_code.as_deref(),
        Some("not_connected"),
        "live empty workspace must be not_connected (not internal / unknown_op): {disconnected:?}"
    );
    assert_eq!(disconnected.data, Value::Null);
    assert_no_token_echo(
        "disconnected ProxyResult",
        &serde_json::to_string(&disconnected).expect("serialize disconnected"),
    );

    let sealed = key_a
        .encrypt(&json!({ "access_token": LEAK, "refresh_token": LEAK }))
        .expect("seal under key A");
    let conn_id = Uuid::now_v7();
    integrations::upsert(
        &pool,
        &UpsertConnection {
            id: conn_id,
            workspace_id,
            provider: "github".into(),
            account_label: Some("iqdec".into()),
            config: json!({ "site": "github.com" }),
            credentials: sealed,
            connected_by: Some(user_id),
            at: Utc::now(),
        },
    )
    .await
    .expect("upsert github sealed under key A");
    assert!(
        integrations::find_connected(&pool, workspace_id, "github")
            .await
            .expect("find_connected after upsert")
            .is_some(),
        "github row must exist before decrypt_failed / control"
    );

    let decrypt_failed = proxy::invoke(
        &pool,
        &key_b,
        ProxyRequest {
            workspace_id,
            provider: "github".into(),
            operation: "list_repos".into(),
            args: stuffed.clone(),
            caller: ProxyCaller::Chat { user_id },
        },
    )
    .await;
    assert!(
        !decrypt_failed.ok,
        "wrong key must be ok:false: {decrypt_failed:?}"
    );
    assert_eq!(
        decrypt_failed.error_code.as_deref(),
        Some("decrypt_failed"),
        "credentials_for Err must map to decrypt_failed: {decrypt_failed:?}"
    );
    assert!(
        decrypt_failed
            .summary
            .contains("failed to decrypt credentials")
            && !decrypt_failed.summary.contains(LEAK),
        "decrypt_failed summary must be the static string and must not contain LEAK: {}",
        decrypt_failed.summary
    );
    assert_eq!(decrypt_failed.data, Value::Null);
    assert_no_token_echo(
        "decrypt_failed ProxyResult",
        &serde_json::to_string(&decrypt_failed).expect("serialize decrypt_failed"),
    );

    let control = proxy::invoke(
        &pool,
        &key_a,
        ProxyRequest {
            workspace_id,
            provider: "github".into(),
            operation: "list_repos".into(),
            args: stuffed,
            caller: ProxyCaller::Chat { user_id },
        },
    )
    .await;
    assert_ne!(
        control.error_code.as_deref(),
        Some("decrypt_failed"),
        "matching key must decrypt (not a tautology that every invoke is decrypt_failed): {control:?}"
    );
    assert_ne!(
        control.error_code.as_deref(),
        Some("not_connected"),
        "matching key must see the connected row: {control:?}"
    );
    assert_ne!(
        control.error_code.as_deref(),
        Some("unknown_op"),
        "list_repos is allowlisted: {control:?}"
    );
    assert_no_token_echo(
        "control ProxyResult",
        &serde_json::to_string(&control).expect("serialize control"),
    );
}
