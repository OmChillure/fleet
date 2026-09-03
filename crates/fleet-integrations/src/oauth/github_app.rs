//! First-time GitHub connect via the [App Manifest] flow.
//!
//! Clicking Connect with no Fleet GitHub app yet POSTs a manifest to GitHub.
//! The user (already on github.com) creates the app in one click, we store
//! client_id/secret, then send them to install + authorize (login + permissions).

use fleet_auth::{AppError, AppResult};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::config::IntegrationsConfig;

/// GitHub App created from a manifest conversion.
#[derive(Debug, Clone)]
pub struct ConvertedGithubApp {
    pub app_id: i64,
    pub slug: String,
    pub client_id: String,
    pub client_secret: String,
    pub webhook_secret: Option<String>,
    pub pem: Option<String>,
}

pub fn manifest_callback_url(cfg: &IntegrationsConfig) -> String {
    format!(
        "{}/api/integrations/oauth/github/manifest/callback",
        cfg.api_public_base
    )
}

pub fn webhook_url(cfg: &IntegrationsConfig) -> String {
    format!("{}/api/integrations/webhooks/github", cfg.api_public_base)
}

/// Unique-enough name (GitHub Apps: max 34 chars).
pub fn app_name_for_workspace(workspace_id: Uuid) -> String {
    let hex = workspace_id.to_string().replace('-', "");
    let tail = hex.get(hex.len().saturating_sub(8)..).unwrap_or("local");
    format!("Agent Fleet {tail}")
}

pub fn manifest_value(cfg: &IntegrationsConfig, workspace_id: Uuid) -> Value {
    let callback = cfg.callback_url("github");
    let mut manifest = json!({
        "name": app_name_for_workspace(workspace_id),
        "url": cfg.ui_public_base,
        "redirect_url": manifest_callback_url(cfg),
        "callback_urls": [callback],
        "setup_url": cfg.callback_url("github"),
        "setup_on_update": false,
        "public": false,
        "default_permissions": {
            "metadata": "read",
            "contents": "write",
            "issues": "write",
            "pull_requests": "write",
            "actions": "read",
            "checks": "read",
            "statuses": "read"
        },
        "default_events": [],
        "request_oauth_on_install": true
    });
    // GitHub rejects localhost / private hook URLs even when active=false.
    let hook = webhook_url(cfg);
    if github_accepts_webhook_url(&hook) {
        manifest["hook_attributes"] = json!({ "url": hook, "active": false });
    }
    manifest
}

/// GitHub App manifests may only set a webhook GitHub can reach on the public Internet.
fn github_accepts_webhook_url(url: &str) -> bool {
    let Some((_, rest)) = url.split_once("://") else {
        return false;
    };
    let hostport = rest.split('/').next().unwrap_or("");
    let host = if let Some(inner) = hostport.strip_prefix('[') {
        inner.split(']').next().unwrap_or("").to_ascii_lowercase()
    } else {
        hostport
            .split(':')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
    };
    if host.is_empty() || host == "localhost" {
        return false;
    }
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        return !ip.is_loopback()
            && !ip.is_private()
            && !ip.is_link_local()
            && !ip.is_unspecified();
    }
    if let Ok(ip) = host.parse::<std::net::Ipv6Addr>() {
        return !ip.is_loopback()
            && !ip.is_unspecified()
            && !ip.is_unique_local()
            && !ip.is_unicast_link_local();
    }
    true
}

pub fn github_install_url(slug: &str, state: &str) -> String {
    format!(
        "https://github.com/apps/{}/installations/new?state={}",
        urlencoding(slug),
        urlencoding(state)
    )
}

pub fn manifest_form_html(cfg: &IntegrationsConfig, workspace_id: Uuid, state: &str) -> String {
    let manifest =
        serde_json::to_string(&manifest_value(cfg, workspace_id)).unwrap_or_else(|_| "{}".into());
    let action = format!(
        "https://github.com/settings/apps/new?state={}",
        urlencoding(state)
    );
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Continue to GitHub</title>
  <style>
    body {{ font: 15px/1.45 system-ui, sans-serif; margin: 2.5rem auto; max-width: 28rem;
           color: #111; }}
    button {{ font: inherit; padding: 0.5rem 1rem; cursor: pointer; }}
  </style>
</head>
<body>
  <p>Continue to GitHub to sign in and grant Agent Fleet access to your repos.</p>
  <form id="fleet-github-manifest" action="{action}" method="post">
    <input type="hidden" name="manifest" value="{manifest}">
    <noscript><button type="submit">Continue to GitHub</button></noscript>
  </form>
  <script>document.getElementById("fleet-github-manifest").submit();</script>
</body>
</html>"#,
        action = html_escape(&action),
        manifest = html_escape(&manifest),
    )
}

#[derive(Debug, Deserialize)]
struct ManifestConversionBody {
    id: i64,
    slug: String,
    client_id: String,
    client_secret: String,
    webhook_secret: Option<String>,
    pem: Option<String>,
    message: Option<String>,
}

pub async fn convert_manifest(code: &str) -> AppResult<ConvertedGithubApp> {
    let http = reqwest::Client::builder()
        .user_agent("agent-fleet")
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| AppError::internal(format!("http client: {e}")))?;

    let url = format!("https://api.github.com/app-manifests/{code}/conversions");
    let res = http
        .post(&url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|e| AppError::internal(format!("GitHub manifest convert failed: {e}")))?;

    let status = res.status();
    let body: Value = res
        .json()
        .await
        .map_err(|e| AppError::internal(format!("GitHub manifest convert parse failed: {e}")))?;

    if !status.is_success() {
        let msg = body
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("could not create GitHub App");
        return Err(AppError::bad_request(format!("GitHub: {msg}")));
    }

    let parsed: ManifestConversionBody = serde_json::from_value(body)
        .map_err(|e| AppError::internal(format!("GitHub manifest convert unexpected body: {e}")))?;
    if parsed.client_id.is_empty() || parsed.client_secret.is_empty() || parsed.slug.is_empty() {
        return Err(AppError::internal(
            "GitHub manifest convert missing credentials",
        ));
    }
    let _ = parsed.message;
    Ok(ConvertedGithubApp {
        app_id: parsed.id,
        slug: parsed.slug,
        client_id: parsed.client_id,
        client_secret: parsed.client_secret,
        webhook_secret: parsed.webhook_secret,
        pem: parsed.pem,
    })
}

fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::IntegrationsConfig;
    use crate::crypto::CredentialsKey;

    fn cfg() -> IntegrationsConfig {
        IntegrationsConfig {
            api_public_base: "http://localhost:8080".into(),
            ui_public_base: "http://localhost:3030".into(),
            state_secret: "dev-change-me-to-a-long-random-secret-at-least-32-chars".into(),
            credentials_key: CredentialsKey::from_secret(
                "dev-change-me-to-a-long-random-secret-at-least-32-chars",
            )
            .expect("key"),
            github: None,
            gitlab: None,
            slack: None,
            linear: None,
            jira: None,
            figma: None,
            notion: None,
            google_drive: None,
            teams: None,
            discord: None,
            trello: None,
            asana: None,
        }
    }

    #[test]
    fn app_name_fits_github_limit() {
        let id = Uuid::parse_str("019fe137-d694-7640-b4f9-8bc187c06f55").unwrap();
        let name = app_name_for_workspace(id);
        assert!(name.len() <= 34, "{name}");
        assert!(name.starts_with("Agent Fleet "));
    }

    #[test]
    fn manifest_points_at_local_callbacks() {
        let id = Uuid::nil();
        let v = manifest_value(&cfg(), id);
        assert_eq!(
            v["redirect_url"],
            "http://localhost:8080/api/integrations/oauth/github/manifest/callback"
        );
        assert_eq!(
            v["callback_urls"][0],
            "http://localhost:8080/api/integrations/oauth/callback/github"
        );
        assert_eq!(v["request_oauth_on_install"], true);
        assert_eq!(v["public"], false);
        assert!(
            v.get("hook_attributes").is_none(),
            "localhost must not set a webhook: {v}"
        );
    }

    #[test]
    fn manifest_omits_private_webhook() {
        assert!(!github_accepts_webhook_url(
            "http://localhost:8080/api/integrations/webhooks/github"
        ));
        assert!(!github_accepts_webhook_url(
            "http://127.0.0.1:8080/api/integrations/webhooks/github"
        ));
        assert!(!github_accepts_webhook_url(
            "http://192.168.1.1:8080/api/integrations/webhooks/github"
        ));
        assert!(github_accepts_webhook_url(
            "https://fleet.example.com/api/integrations/webhooks/github"
        ));
        let mut public = cfg();
        public.api_public_base = "https://fleet.example.com".into();
        let v = manifest_value(&public, Uuid::nil());
        assert_eq!(
            v["hook_attributes"]["url"],
            "https://fleet.example.com/api/integrations/webhooks/github"
        );
    }

    #[test]
    fn form_html_includes_manifest_and_github_action() {
        let html = manifest_form_html(&cfg(), Uuid::nil(), "state-token");
        assert!(html.contains("github.com/settings/apps/new"));
        assert!(html.contains("name=\"manifest\""));
        assert!(html.contains("fleet-github-manifest"));
    }

    #[test]
    fn install_url_encodes_state() {
        let url = github_install_url("agent-fleet-deadbeef", "a+b");
        assert!(url.starts_with("https://github.com/apps/agent-fleet-deadbeef/installations/new?"));
        assert!(url.contains("state=a%2Bb"));
    }

    fn user_oauth_client() -> crate::config::OAuthClientConfig {
        crate::config::OAuthClientConfig {
            client_id: "Iv1.user-oauth".into(),
            client_secret: "user-oauth-secret".into(),
            github_app_slug: None,
        }
    }

    fn user_oauth_authorize_url(state: &str) -> String {
        crate::oauth::authorize_url("github", &cfg(), &user_oauth_client(), state)
            .expect("github user-oauth authorize url")
    }

    fn encode_state_typ(secret: &str, typ: &str) -> String {
        use super::super::state_token::OAuthStateClaims;
        use jsonwebtoken::{EncodingKey, Header, encode};

        let now = chrono::Utc::now().timestamp();
        encode(
            &Header::default(),
            &OAuthStateClaims {
                sub: Uuid::now_v7().to_string(),
                workspace_id: Uuid::now_v7().to_string(),
                provider: "github".into(),
                nonce: format!("nonce-{typ}"),
                exp: now + 600,
                iat: now,
                typ: typ.into(),
            },
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .expect("encode typed oauth state")
    }

    #[test]
    fn github_app_install_url_is_not_user_oauth_authorize_url() {
        let state = "state-token";
        let install = github_install_url("agent-fleet-deadbeef", state);
        let authorize = user_oauth_authorize_url(state);

        assert_ne!(install, authorize);
        assert!(
            install.starts_with("https://github.com/apps/agent-fleet-deadbeef/installations/new?"),
            "{install}"
        );
        assert!(
            authorize.starts_with("https://github.com/login/oauth/authorize?"),
            "{authorize}"
        );
        assert!(
            !install.contains("/login/oauth/authorize"),
            "install URL must not be the user-oauth authorize endpoint: {install}"
        );
        assert!(
            !authorize.contains("/installations/new"),
            "user-oauth authorize must not be the App install endpoint: {authorize}"
        );
        assert!(
            !authorize.contains("/settings/apps/new"),
            "user-oauth authorize must not be the App manifest endpoint: {authorize}"
        );
    }

    #[test]
    fn manifest_form_action_is_not_user_oauth_authorize_url() {
        let state = "state-token";
        let html = manifest_form_html(&cfg(), Uuid::nil(), state);
        let authorize = user_oauth_authorize_url(state);

        assert!(
            html.contains("https://github.com/settings/apps/new?state=state-token"),
            "{html}"
        );
        assert!(
            !html.contains("/login/oauth/authorize"),
            "manifest form must not POST to user-oauth authorize: {html}"
        );
        assert!(
            !html.contains("/installations/new"),
            "manifest form is create-app, not install: {html}"
        );
        assert!(
            authorize.starts_with("https://github.com/login/oauth/authorize?"),
            "{authorize}"
        );
        assert!(!authorize.contains("/settings/apps/new"));
    }

    #[test]
    fn authorize_with_app_slug_uses_install_url_not_user_oauth() {
        let state = "state-token";
        let slug_client = crate::config::OAuthClientConfig {
            client_id: "Iv1.app".into(),
            client_secret: "app-secret".into(),
            github_app_slug: Some("agent-fleet-deadbeef".into()),
        };
        let with_slug = crate::oauth::authorize_url("github", &cfg(), &slug_client, state)
            .expect("github app install start");
        let without_slug = user_oauth_authorize_url(state);

        assert_eq!(with_slug, github_install_url("agent-fleet-deadbeef", state));
        assert_ne!(with_slug, without_slug);
        assert!(!with_slug.contains("/login/oauth/authorize"));
    }

    #[test]
    fn manifest_callback_is_not_user_oauth_callback() {
        let c = cfg();
        let manifest_cb = manifest_callback_url(&c);
        let user_cb = c.callback_url("github");
        assert_ne!(manifest_cb, user_cb);
        assert_eq!(
            manifest_cb,
            "http://localhost:8080/api/integrations/oauth/github/manifest/callback"
        );
        assert_eq!(
            user_cb,
            "http://localhost:8080/api/integrations/oauth/callback/github"
        );
        assert!(
            !manifest_cb.ends_with("/oauth/callback/github"),
            "manifest callback must not be the user-oauth callback: {manifest_cb}"
        );
        assert!(
            !user_cb.contains("manifest"),
            "user-oauth callback must not be the manifest callback: {user_cb}"
        );

        let v = manifest_value(&c, Uuid::nil());
        assert_eq!(v["redirect_url"], manifest_cb);
        assert_eq!(v["callback_urls"][0], user_cb);
        assert_eq!(v["setup_url"], user_cb);

        let authorize = user_oauth_authorize_url("state-token");
        assert!(
            authorize.contains("redirect_uri=") && authorize.contains("oauth%2Fcallback%2Fgithub"),
            "user-oauth authorize must send GitHub to the user-oauth callback: {authorize}"
        );
        assert!(
            !authorize.contains("manifest"),
            "user-oauth authorize must not send GitHub to the manifest callback: {authorize}"
        );
    }

    #[test]
    fn manifest_state_cannot_complete_user_oauth_callback() {
        let secret = cfg().state_secret.clone();
        let manifest_state = encode_state_typ(&secret, "github_manifest");
        let err = crate::oauth::verify_oauth_state(&secret, &manifest_state).unwrap_err();
        match err {
            fleet_auth::AppError::Unauthorized(msg) => {
                assert!(
                    msg.contains("type") || msg.contains("invalid"),
                    "manifest state must not verify as user-oauth state, got {msg}"
                );
            }
            other => panic!("expected Unauthorized, got {other:?}"),
        }
    }

    #[test]
    fn user_oauth_state_cannot_complete_manifest_callback() {
        use super::super::state_token::OAuthStateClaims;
        use jsonwebtoken::{DecodingKey, Validation, decode};

        let secret = cfg().state_secret.clone();
        let user_state =
            crate::oauth::issue_oauth_state(&secret, Uuid::now_v7(), Uuid::now_v7(), "github")
                .unwrap();
        let user_claims = crate::oauth::verify_oauth_state(&secret, &user_state)
            .expect("user-oauth state verifies for the user-oauth callback");
        assert_eq!(user_claims.typ, "oauth_state");

        let manifest_state = encode_state_typ(&secret, "github_manifest");
        let mut validation = Validation::default();
        validation.validate_exp = true;
        let manifest_claims = decode::<OAuthStateClaims>(
            &manifest_state,
            &DecodingKey::from_secret(secret.as_bytes()),
            &validation,
        )
        .expect("manifest-typed jwt decodes")
        .claims;
        assert_eq!(manifest_claims.typ, "github_manifest");
        assert_ne!(
            user_claims.typ, manifest_claims.typ,
            "user-oauth typ must not be accepted as a GitHub App manifest state"
        );

        // Same JWT string cannot be the state for both GitHub start URLs
        // without landing on different Fleet callbacks.
        let c = cfg();
        assert_ne!(manifest_callback_url(&c), c.callback_url("github"));
        let html = manifest_form_html(&c, Uuid::nil(), &manifest_state);
        let authorize = user_oauth_authorize_url(&user_state);
        assert!(html.contains("/settings/apps/new"));
        assert!(authorize.contains("/login/oauth/authorize"));
        assert!(!html.contains("/login/oauth/authorize"));
        assert!(!authorize.contains("/settings/apps/new"));
    }
}
