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
    format!(
        "{}/api/integrations/webhooks/github",
        cfg.api_public_base
    )
}

/// Unique-enough name (GitHub Apps: max 34 chars).
pub fn app_name_for_workspace(workspace_id: Uuid) -> String {
    let hex = workspace_id.to_string().replace('-', "");
    let tail = hex
        .get(hex.len().saturating_sub(8)..)
        .unwrap_or("local");
    format!("Agent Fleet {tail}")
}

pub fn manifest_value(cfg: &IntegrationsConfig, workspace_id: Uuid) -> Value {
    let callback = cfg.callback_url("github");
    json!({
        "name": app_name_for_workspace(workspace_id),
        "url": cfg.ui_public_base,
        "hook_attributes": {
            "url": webhook_url(cfg),
            "active": false
        },
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
    })
}

pub fn github_install_url(slug: &str, state: &str) -> String {
    format!(
        "https://github.com/apps/{}/installations/new?state={}",
        urlencoding(slug),
        urlencoding(state)
    )
}

pub fn manifest_form_html(cfg: &IntegrationsConfig, workspace_id: Uuid, state: &str) -> String {
    let manifest = serde_json::to_string(&manifest_value(cfg, workspace_id))
        .unwrap_or_else(|_| "{}".into());
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

    let parsed: ManifestConversionBody = serde_json::from_value(body).map_err(|e| {
        AppError::internal(format!("GitHub manifest convert unexpected body: {e}"))
    })?;
    if parsed.client_id.is_empty() || parsed.client_secret.is_empty() || parsed.slug.is_empty() {
        return Err(AppError::internal("GitHub manifest convert missing credentials"));
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
    use crate::crypto::CredentialsKey;
    use crate::config::IntegrationsConfig;

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
}
