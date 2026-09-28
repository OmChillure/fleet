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
        "default_events": ["issues", "issue_comment", "pull_request"],
        "request_oauth_on_install": true
    });
    // GitHub rejects localhost / private hook URLs even when active=false.
    let hook = webhook_url(cfg);
    if github_accepts_webhook_url(&hook) {
        manifest["hook_attributes"] = json!({ "url": hook, "active": true });
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
            gitea: None,
            gitea_base_url: "http://localhost:3033".into(),
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
            bitbucket: None,
            confluence: None,
        }
    }

    #[test]
    fn app_name_fits_github_limit() {
        let id = Uuid::parse_str("019fe137-d694-7640-b4f9-8bc187c06f55").unwrap();
        let name = app_name_for_workspace(id);
        assert_eq!(name, "Agent Fleet 87c06f55");
        assert!(name.len() <= 34, "{name}");
    }

    #[test]
    fn app_names_differ_across_workspaces() {
        let a = Uuid::parse_str("019fe137-d694-7640-b4f9-8bc187c06f55").unwrap();
        let b = Uuid::parse_str("019fe137-d694-7640-b4f9-aaaaaaaaaaaa").unwrap();
        assert_ne!(app_name_for_workspace(a), app_name_for_workspace(b));
    }

    #[test]
    fn manifest_name_is_app_name_for_workspace() {
        let id = Uuid::parse_str("019fe137-d694-7640-b4f9-8bc187c06f55").unwrap();
        let name = app_name_for_workspace(id);
        let v = manifest_value(&cfg(), id);
        assert_eq!(v["name"], name);
        assert!(name.len() <= 34, "{name}");
    }

    #[test]
    fn manifest_points_at_local_callbacks() {
        let v = manifest_value(&cfg(), Uuid::nil());
        assert_eq!(v["request_oauth_on_install"], true);
        assert_eq!(v["public"], false);
        assert!(
            v.get("hook_attributes").is_none(),
            "localhost must not set a webhook: {v}"
        );
    }

    #[test]
    fn manifest_homepage_url_is_ui_public_base() {
        let c = cfg();
        let v = manifest_value(&c, Uuid::nil());
        assert_eq!(v["url"], c.ui_public_base);
        assert_eq!(v["url"], "http://localhost:3030");
    }

    #[test]
    fn manifest_default_permissions_include_repo_write() {
        let perms = &manifest_value(&cfg(), Uuid::nil())["default_permissions"];
        assert_eq!(perms["contents"], "write");
        assert_eq!(perms["issues"], "write");
        assert_eq!(perms["pull_requests"], "write");
        assert_eq!(perms["metadata"], "read");
        assert_eq!(perms["actions"], "read");
        assert_eq!(perms["checks"], "read");
        assert_eq!(perms["statuses"], "read");
    }

    #[test]
    fn public_api_base_manifest_callbacks_use_that_host() {
        let mut public = cfg();
        public.api_public_base = "https://fleet.example.com".into();
        let v = manifest_value(&public, Uuid::nil());
        assert_eq!(
            v["redirect_url"],
            "https://fleet.example.com/api/integrations/oauth/github/manifest/callback"
        );
        assert_eq!(
            v["callback_urls"][0],
            "https://fleet.example.com/api/integrations/oauth/callback/github"
        );
    }

    #[test]
    fn manifest_omits_private_webhook() {
        let mut loopback = cfg();
        loopback.api_public_base = "http://127.0.0.1:8080".into();
        let loopback_manifest = manifest_value(&loopback, Uuid::nil());
        assert!(
            loopback_manifest.get("hook_attributes").is_none(),
            "127.0.0.1 must not set a webhook: {loopback_manifest}"
        );

        let mut rfc1918 = cfg();
        rfc1918.api_public_base = "http://192.168.1.1:8080".into();
        let rfc1918_manifest = manifest_value(&rfc1918, Uuid::nil());
        assert!(
            rfc1918_manifest.get("hook_attributes").is_none(),
            "RFC1918 must not set a webhook: {rfc1918_manifest}"
        );

        let mut public = cfg();
        public.api_public_base = "https://fleet.example.com".into();
        let public_manifest = manifest_value(&public, Uuid::nil());
        assert_eq!(
            public_manifest["hook_attributes"]["url"],
            "https://fleet.example.com/api/integrations/webhooks/github"
        );
        assert_eq!(public_manifest["hook_attributes"]["active"], true);
    }

    #[test]
    fn manifest_omits_hook_attributes_for_ipv6_loopback_and_ula() {
        let mut loopback = cfg();
        loopback.api_public_base = "http://[::1]:8080".into();
        let loopback_manifest = manifest_value(&loopback, Uuid::nil());
        assert!(
            loopback_manifest.get("hook_attributes").is_none(),
            "IPv6 loopback must not set a webhook: {loopback_manifest}"
        );

        let mut ula = cfg();
        ula.api_public_base = "https://[fc00::1]".into();
        let ula_manifest = manifest_value(&ula, Uuid::nil());
        assert!(
            ula_manifest.get("hook_attributes").is_none(),
            "IPv6 unique local must not set a webhook: {ula_manifest}"
        );
    }

    #[test]
    fn manifest_omits_hook_attributes_for_link_local_and_unspecified() {
        let mut link_local = cfg();
        link_local.api_public_base = "http://169.254.1.1:8080".into();
        let link_local_manifest = manifest_value(&link_local, Uuid::nil());
        assert!(
            link_local_manifest.get("hook_attributes").is_none(),
            "IPv4 link-local must not set a webhook: {link_local_manifest}"
        );

        let mut unspecified = cfg();
        unspecified.api_public_base = "http://0.0.0.0:8080".into();
        let unspecified_manifest = manifest_value(&unspecified, Uuid::nil());
        assert!(
            unspecified_manifest.get("hook_attributes").is_none(),
            "unspecified IPv4 must not set a webhook: {unspecified_manifest}"
        );

        let mut v6_link_local = cfg();
        v6_link_local.api_public_base = "http://[fe80::1]".into();
        let v6_link_local_manifest = manifest_value(&v6_link_local, Uuid::nil());
        assert!(
            v6_link_local_manifest.get("hook_attributes").is_none(),
            "IPv6 link-local must not set a webhook: {v6_link_local_manifest}"
        );
    }

    #[test]
    fn manifest_sets_default_events_issues_pr() {
        let v = manifest_value(&cfg(), Uuid::nil());
        assert_eq!(
            v["default_events"],
            json!(["issues", "issue_comment", "pull_request"])
        );
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

    #[test]
    fn manifest_form_action_is_not_user_oauth_authorize_url() {
        let html = manifest_form_html(&cfg(), Uuid::nil(), "state-token");
        assert!(
            html.contains("https://github.com/settings/apps/new?state=state-token"),
            "{html}"
        );
    }

    #[test]
    fn manifest_form_action_urlencodes_state() {
        let html = manifest_form_html(&cfg(), Uuid::nil(), "a+b");
        assert!(
            html.contains("https://github.com/settings/apps/new?state=a%2Bb"),
            "{html}"
        );
    }

    #[test]
    fn manifest_form_hidden_field_equals_manifest_value() {
        let c = cfg();
        let id = Uuid::nil();
        let html = manifest_form_html(&c, id, "state-token");
        let marker = "name=\"manifest\" value=\"";
        let start = html
            .find(marker)
            .unwrap_or_else(|| panic!("hidden name=manifest: {html}"))
            + marker.len();
        let end = html[start..]
            .find('"')
            .unwrap_or_else(|| panic!("manifest value close quote: {html}"));
        let raw = html[start..start + end]
            .replace("&quot;", "\"")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&");
        let got: serde_json::Value =
            serde_json::from_str(&raw).unwrap_or_else(|e| panic!("manifest JSON: {e}: {raw}"));
        assert_eq!(got, manifest_value(&c, id));
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
        let v = manifest_value(&cfg(), Uuid::nil());
        assert_eq!(
            v["setup_url"],
            "http://localhost:8080/api/integrations/oauth/callback/github"
        );

        let authorize = user_oauth_authorize_url("state-token");
        assert!(
            authorize.contains(
                "redirect_uri=http%3A%2F%2Flocalhost%3A8080%2Fapi%2Fintegrations%2Foauth%2Fcallback%2Fgithub"
            ),
            "user-oauth authorize must send GitHub to the user-oauth callback: {authorize}"
        );
        assert!(
            !authorize.contains("manifest"),
            "user-oauth authorize must not send GitHub to the manifest callback: {authorize}"
        );
    }
}
