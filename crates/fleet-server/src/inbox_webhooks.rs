//! Provider → inbox ingest. No user cookie.
//!
//! `POST /api/integrations/webhooks/{provider}/{hook_token}` is the pasteable
//! URL. GitHub Apps also hit `/api/integrations/webhooks/github` and we match
//! `X-Hub-Signature-256` against stored app webhook secrets.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use uuid::Uuid;

use crate::inbox_sources;
use crate::state::AppState;
use crate::tickets::{self, IngestItem};

type HmacSha256 = Hmac<Sha256>;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/integrations/webhooks/{provider}",
            get(http_info).post(http_post),
        )
        .route(
            "/api/integrations/webhooks/{provider}/{token}",
            get(http_info_token).post(http_post_token),
        )
}

async fn http_info(Path(provider): Path<String>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "provider": provider.trim().to_ascii_lowercase(),
        "hint": "POST events here. Prefer /{provider}/{hook_token} from Inbox.",
    }))
}

async fn http_info_token(Path((provider, _token)): Path<(String, String)>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "provider": provider.trim().to_ascii_lowercase(),
    }))
}

async fn http_post(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle(&state, &provider, None, &headers, &body).await
}

async fn http_post_token(
    State(state): State<AppState>,
    Path((provider, token)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle(&state, &provider, Some(token.as_str()), &headers, &body).await
}

async fn handle(
    state: &AppState,
    provider: &str,
    token: Option<&str>,
    headers: &HeaderMap,
    body: &[u8],
) -> Response {
    let provider = provider.trim().to_ascii_lowercase();
    if !crate::inbox_sources::is_inbox_provider(&provider) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "unknown inbox provider" })),
        )
            .into_response();
    }

    if let Some(challenge) = slack_challenge(body) {
        if provider != "slack" {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "not a slack endpoint" })),
            )
                .into_response();
        }
        match resolve_workspace(state, &provider, token, headers, body).await {
            Ok(Some(_)) => return (StatusCode::OK, Json(challenge)).into_response(),
            _ => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({ "error": "unknown webhook token or signature" })),
                )
                    .into_response();
            }
        }
    }

    let workspace_id = match resolve_workspace(state, &provider, token, headers, body).await {
        Ok(Some(id)) => id,
        Ok(None) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "unknown webhook token or signature" })),
            )
                .into_response();
        }
        Err(e) => {
            tracing::warn!(provider = %provider, error = %e, "webhook resolve failed");
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "webhook not authorized" })),
            )
                .into_response();
        }
    };

    let parsed = match parse_event(&provider, headers, body) {
        Ok(ParsedEvent::Ignore | ParsedEvent::Ping) => {
            return StatusCode::NO_CONTENT.into_response();
        }
        Ok(ParsedEvent::Items(items)) => items,
        Err(e) => {
            tracing::warn!(provider = %provider, error = %e, "webhook parse failed");
            return (StatusCode::BAD_REQUEST, Json(json!({ "error": e }))).into_response();
        }
    };

    match tickets::ingest_webhook_items(state, workspace_id, parsed).await {
        Ok(report) => (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "workspace_id": workspace_id,
                "ingested": report.ingested,
                "launched": report.launched,
            })),
        )
            .into_response(),
        Err(e) => {
            tracing::warn!(%workspace_id, provider = %provider, error = %e, "webhook ingest failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "ingest failed" })),
            )
                .into_response()
        }
    }
}

async fn resolve_workspace(
    state: &AppState,
    provider: &str,
    token: Option<&str>,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<Option<Uuid>, String> {
    if let Some(token) = token.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(mon) = fleet_db::repos::tickets::get_monitor_by_hook_token(&state.db, token)
            .await
            .map_err(|e| e.to_string())?
        {
            if !inbox_sources::webhook_provider_allowed(
                mon.follow_connected,
                &mon.providers,
                provider,
            ) {
                return Ok(None);
            }
            if mon.follow_connected && mon.providers.is_empty() {
                let connected = fleet_db::repos::integrations::find_connected(
                    &state.db,
                    mon.workspace_id,
                    provider,
                )
                .await
                .map_err(|e| e.to_string())?;
                if connected.is_none() {
                    return Ok(None);
                }
            }
            if !token_path_signature_ok(state, provider, mon.workspace_id, token, headers, body)
                .await?
            {
                return Ok(None);
            }
            return Ok(Some(mon.workspace_id));
        }
        return Ok(None);
    }

    if provider == "github" {
        if let Some(ws) = match_github_app_signature(state, headers, body).await? {
            return Ok(Some(ws));
        }
    }
    Ok(None)
}

async fn match_github_app_signature(
    state: &AppState,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<Option<Uuid>, String> {
    let Some(sig) = header_str(headers, "x-hub-signature-256") else {
        return Ok(None);
    };
    let apps = fleet_db::repos::integrations::list_oauth_apps_for_provider(&state.db, "github")
        .await
        .map_err(|e| e.to_string())?;
    for app in apps {
        let Ok(plain) = state
            .integrations
            .credentials_key
            .decrypt(&app.client_secret)
        else {
            continue;
        };
        let Some(secret) = plain
            .get("webhook_secret")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        if github_signature_ok(secret, sig, body) {
            return Ok(Some(app.workspace_id));
        }
    }
    Ok(None)
}

/// Path token is the capability. Providers with a signing scheme must present
/// a valid signature. Missing header / missing secret / decrypt failure deny.
async fn token_path_signature_ok(
    state: &AppState,
    provider: &str,
    workspace_id: Uuid,
    token: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<bool, String> {
    match provider {
        "gitlab" => Ok(match header_str(headers, "x-gitlab-token") {
            Some(got) => secure_eq(got.trim(), token),
            None => false,
        }),
        "gitea" => {
            let sig = header_str(headers, "x-gitea-signature")
                .or_else(|| header_str(headers, "x-hub-signature-256"));
            Ok(match sig {
                Some(sig) => github_signature_ok(token, sig, body),
                None => false,
            })
        }
        "github" | "linear" | "slack" => {
            let stored = provider_signing_secret(state, workspace_id, provider).await?;
            let Some(secret) = stored else {
                return Ok(false);
            };
            Ok(match provider {
                "github" => match header_str(headers, "x-hub-signature-256") {
                    Some(sig) => github_signature_ok(&secret, sig, body),
                    None => false,
                },
                "linear" => match header_str(headers, "linear-signature") {
                    Some(sig) => linear_signature_ok(&secret, sig, body),
                    None => false,
                },
                "slack" => match header_str(headers, "x-slack-signature") {
                    Some(sig) => slack_signature_ok(&secret, headers, sig, body),
                    None => false,
                },
                _ => false,
            })
        }
        // Token + provider bind (checked in resolve_workspace) is the capability.
        _ => Ok(true),
    }
}

async fn provider_signing_secret(
    state: &AppState,
    workspace_id: Uuid,
    provider: &str,
) -> Result<Option<String>, String> {
    let Some(app) = fleet_db::repos::integrations::get_oauth_app(&state.db, workspace_id, provider)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let plain = state
        .integrations
        .credentials_key
        .decrypt(&app.client_secret)
        .map_err(|_| "webhook signing secret unavailable".to_string())?;
    let secret = plain
        .get("webhook_secret")
        .or_else(|| plain.get("signing_secret"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    Ok(secret)
}

fn github_signature_ok(secret: &str, header: &str, body: &[u8]) -> bool {
    let got = header
        .trim()
        .strip_prefix("sha256=")
        .unwrap_or(header.trim());
    let expect = hmac_hex(secret.as_bytes(), body);
    secure_eq(got, &expect)
}

fn linear_signature_ok(secret: &str, header: &str, body: &[u8]) -> bool {
    let expect = hmac_hex(secret.as_bytes(), body);
    secure_eq(header.trim(), &expect)
}

fn slack_timestamp_fresh(headers: &HeaderMap) -> bool {
    slack_timestamp_fresh_at(headers, chrono::Utc::now().timestamp())
}

fn slack_timestamp_fresh_at(headers: &HeaderMap, now: i64) -> bool {
    let Some(ts) = header_str(headers, "x-slack-request-timestamp") else {
        return false;
    };
    let Ok(ts) = ts.trim().parse::<i64>() else {
        return false;
    };
    (now - ts).unsigned_abs() <= 300
}

fn slack_signature_ok(secret: &str, headers: &HeaderMap, header: &str, body: &[u8]) -> bool {
    if !slack_timestamp_fresh(headers) {
        return false;
    }
    let Some(ts) = header_str(headers, "x-slack-request-timestamp") else {
        return false;
    };
    let mut msg = Vec::from(b"v0:");
    msg.extend_from_slice(ts.as_bytes());
    msg.push(b':');
    msg.extend_from_slice(body);
    let expect = format!("v0={}", hmac_hex(secret.as_bytes(), &msg));
    secure_eq(header.trim(), &expect)
}

fn hmac_hex(secret: &[u8], body: &[u8]) -> String {
    if secret.is_empty() {
        return String::new();
    }
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC-SHA256 accepts any key length");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

fn secure_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn slack_challenge(body: &[u8]) -> Option<Value> {
    let v: Value = serde_json::from_slice(body).ok()?;
    if v.get("type").and_then(|t| t.as_str()) == Some("url_verification") {
        if let Some(c) = v.get("challenge").cloned() {
            return Some(json!({ "challenge": c }));
        }
    }
    None
}

#[derive(Debug)]
pub enum ParsedEvent {
    Ping,
    Ignore,
    Items(Vec<IngestItem>),
}

pub fn parse_event(
    provider: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<ParsedEvent, String> {
    if body.is_empty() {
        return Ok(ParsedEvent::Ignore);
    }
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("json: {e}"))?;
    match provider {
        "github" => parse_github(headers, &v),
        "gitlab" => parse_gitlab(&v),
        "gitea" => parse_gitea(headers, &v),
        "jira" => parse_jira(&v),
        "linear" => parse_linear(&v),
        "slack" => parse_slack(&v),
        "trello" => parse_trello(&v),
        "asana" => parse_asana(&v),
        "notion" => parse_notion(&v),
        "bitbucket" => parse_bitbucket(&v),
        "teams" => parse_teams(&v),
        "confluence" => parse_confluence(&v),
        _ => Err(format!("no webhook parser for {provider}")),
    }
}

fn parse_github(headers: &HeaderMap, v: &Value) -> Result<ParsedEvent, String> {
    let event = header_str(headers, "x-github-event").unwrap_or("");
    if event == "ping" || v.get("zen").is_some() && v.get("hook_id").is_some() {
        return Ok(ParsedEvent::Ping);
    }
    let repo = v
        .pointer("/repository/full_name")
        .and_then(|x| x.as_str())
        .unwrap_or("");
    if event == "issues" {
        let issue = v.get("issue").ok_or("missing issue")?;
        if issue.get("pull_request").is_some() {
            return Ok(ParsedEvent::Ignore);
        }
        let number = issue
            .get("number")
            .and_then(|n| n.as_u64())
            .ok_or("no number")?;
        if repo.is_empty() {
            return Err("missing repository.full_name".into());
        }
        let state = issue
            .get("state")
            .and_then(|s| s.as_str())
            .unwrap_or("open");
        return Ok(ParsedEvent::Items(vec![IngestItem {
            provider: "github".into(),
            key: format!("{repo}#{number}"),
            title: issue
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("untitled")
                .to_string(),
            url: issue
                .get("html_url")
                .and_then(|u| u.as_str())
                .map(ToOwned::to_owned),
            summary: issue
                .get("body")
                .and_then(|b| b.as_str())
                .unwrap_or("")
                .chars()
                .take(4000)
                .collect(),
            closed: state == "closed",
        }]));
    }
    if event == "pull_request" {
        let pr = v.get("pull_request").ok_or("missing pull_request")?;
        let number = pr
            .get("number")
            .and_then(|n| n.as_u64())
            .ok_or("no number")?;
        if repo.is_empty() {
            return Err("missing repository.full_name".into());
        }
        let state = pr.get("state").and_then(|s| s.as_str()).unwrap_or("open");
        return Ok(ParsedEvent::Items(vec![IngestItem {
            provider: "github".into(),
            key: format!("{repo}#{number}"),
            title: pr
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("untitled")
                .to_string(),
            url: pr
                .get("html_url")
                .and_then(|u| u.as_str())
                .map(ToOwned::to_owned),
            summary: pr
                .get("body")
                .and_then(|b| b.as_str())
                .unwrap_or("")
                .chars()
                .take(4000)
                .collect(),
            closed: state == "closed" || state == "merged",
        }]));
    }
    if event == "issue_comment" {
        let issue = v.get("issue").ok_or("missing issue")?;
        let number = issue
            .get("number")
            .and_then(|n| n.as_u64())
            .ok_or("no number")?;
        if repo.is_empty() {
            return Err("missing repository.full_name".into());
        }
        let comment = v
            .pointer("/comment/body")
            .and_then(|b| b.as_str())
            .unwrap_or("");
        return Ok(ParsedEvent::Items(vec![IngestItem {
            provider: "github".into(),
            key: format!("{repo}#{number}"),
            title: issue
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("untitled")
                .to_string(),
            url: issue
                .get("html_url")
                .and_then(|u| u.as_str())
                .map(ToOwned::to_owned),
            summary: comment.chars().take(4000).collect(),
            closed: issue.get("state").and_then(|s| s.as_str()) == Some("closed"),
        }]));
    }
    Ok(ParsedEvent::Ignore)
}

fn parse_gitea(headers: &HeaderMap, v: &Value) -> Result<ParsedEvent, String> {
    let event = header_str(headers, "x-gitea-event")
        .or_else(|| header_str(headers, "x-github-event"))
        .unwrap_or("");
    if event == "ping" {
        return Ok(ParsedEvent::Ping);
    }
    let mut forwarded = headers.clone();
    if header_str(&forwarded, "x-github-event").is_none() && !event.is_empty() {
        if let Ok(value) = HeaderValue::from_str(event) {
            forwarded.insert("x-github-event", value);
        }
    }
    let mut parsed = parse_github(&forwarded, v)?;
    if let ParsedEvent::Items(items) = &mut parsed {
        for item in items.iter_mut() {
            item.provider = "gitea".into();
        }
    }
    Ok(parsed)
}

fn parse_gitlab(v: &Value) -> Result<ParsedEvent, String> {
    let kind = v
        .get("object_kind")
        .or_else(|| v.get("event_type"))
        .and_then(|k| k.as_str())
        .unwrap_or("");
    let attrs = v.get("object_attributes").unwrap_or(v);
    let project = v
        .pointer("/project/path_with_namespace")
        .or_else(|| v.pointer("/project/path"))
        .and_then(|p| p.as_str())
        .unwrap_or("");
    let iid = attrs
        .get("iid")
        .or_else(|| attrs.get("id"))
        .and_then(|n| n.as_u64())
        .or_else(|| attrs.get("iid").and_then(|n| n.as_i64()).map(|n| n as u64));
    if !matches!(kind, "issue" | "merge_request") || iid.is_none() || project.is_empty() {
        return Ok(ParsedEvent::Ignore);
    }
    if let Some(iid) = iid {
        let key = format!("{project}#{iid}");
        let state = attrs
            .get("state")
            .and_then(|s| s.as_str())
            .unwrap_or("opened");
        return Ok(ParsedEvent::Items(vec![IngestItem {
            provider: "gitlab".into(),
            key,
            title: attrs
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("untitled")
                .to_string(),
            url: attrs
                .get("url")
                .and_then(|u| u.as_str())
                .map(ToOwned::to_owned),
            summary: attrs
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .chars()
                .take(4000)
                .collect(),
            closed: matches!(state, "closed" | "merged"),
        }]));
    }
    Ok(ParsedEvent::Ignore)
}

fn parse_jira(v: &Value) -> Result<ParsedEvent, String> {
    let issue = v.get("issue").ok_or("missing issue")?;
    let key = issue
        .get("key")
        .and_then(|k| k.as_str())
        .ok_or("missing issue.key")?;
    let fields = issue.get("fields").cloned().unwrap_or(json!({}));
    let title = fields
        .get("summary")
        .and_then(|s| s.as_str())
        .unwrap_or(key)
        .to_string();
    let status = fields
        .pointer("/status/name")
        .and_then(|s| s.as_str())
        .unwrap_or("");
    Ok(ParsedEvent::Items(vec![IngestItem {
        provider: "jira".into(),
        key: key.to_string(),
        title,
        url: None,
        summary: format!("Jira status: {status}"),
        closed: tickets::status_looks_done(status),
    }]))
}

fn parse_linear(v: &Value) -> Result<ParsedEvent, String> {
    let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("Issue");
    if !ty.eq_ignore_ascii_case("issue")
        && v.get("data").and_then(|d| d.get("identifier")).is_none()
    {
        return Ok(ParsedEvent::Ignore);
    }
    let data = v.get("data").unwrap_or(v);
    let key = data
        .get("identifier")
        .and_then(|k| k.as_str())
        .ok_or("missing identifier")?;
    let state_type = data
        .pointer("/state/type")
        .or_else(|| data.get("state_type"))
        .and_then(|s| s.as_str())
        .unwrap_or("");
    Ok(ParsedEvent::Items(vec![IngestItem {
        provider: "linear".into(),
        key: key.to_string(),
        title: data
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or(key)
            .to_string(),
        url: data
            .get("url")
            .and_then(|u| u.as_str())
            .map(ToOwned::to_owned),
        summary: data
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or("")
            .chars()
            .take(4000)
            .collect(),
        closed: matches!(state_type, "completed" | "canceled"),
    }]))
}

fn parse_slack(v: &Value) -> Result<ParsedEvent, String> {
    let ev = v.get("event").unwrap_or(v);
    if ev.get("type").and_then(|t| t.as_str()) != Some("message") {
        return Ok(ParsedEvent::Ignore);
    }
    let channel = ev.get("channel").and_then(|c| c.as_str()).unwrap_or("");
    let Some((key, title, summary)) = crate::inbox_sources::slack_inbox_item(channel, ev) else {
        return Ok(ParsedEvent::Ignore);
    };
    Ok(ParsedEvent::Items(vec![IngestItem {
        provider: "slack".into(),
        key,
        title,
        url: None,
        summary,
        closed: false,
    }]))
}

fn parse_trello(v: &Value) -> Result<ParsedEvent, String> {
    let card = v
        .pointer("/action/data/card")
        .or_else(|| v.get("card"))
        .cloned()
        .ok_or("missing card")?;
    let key = card
        .get("id")
        .and_then(|i| i.as_str())
        .ok_or("missing card id")?;
    Ok(ParsedEvent::Items(vec![IngestItem {
        provider: "trello".into(),
        key: key.to_string(),
        title: card
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("card")
            .to_string(),
        url: card
            .get("url")
            .or_else(|| card.get("shortLink"))
            .and_then(|u| u.as_str())
            .map(ToOwned::to_owned),
        summary: card
            .get("desc")
            .and_then(|d| d.as_str())
            .unwrap_or("")
            .chars()
            .take(4000)
            .collect(),
        closed: card
            .get("closed")
            .and_then(|c| c.as_bool())
            .unwrap_or(false),
    }]))
}

fn parse_asana(v: &Value) -> Result<ParsedEvent, String> {
    let ev = v
        .get("events")
        .and_then(|e| e.as_array())
        .and_then(|a| a.first())
        .unwrap_or(v);
    let resource = ev.get("resource").unwrap_or(ev);
    let key = resource
        .get("gid")
        .or_else(|| resource.get("id"))
        .and_then(|g| {
            g.as_str()
                .map(ToOwned::to_owned)
                .or_else(|| g.as_u64().map(|n| n.to_string()))
        })
        .ok_or("missing asana gid")?;
    Ok(ParsedEvent::Items(vec![IngestItem {
        provider: "asana".into(),
        key,
        title: resource
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("task")
            .to_string(),
        url: None,
        summary: String::new(),
        closed: false,
    }]))
}

fn parse_notion(v: &Value) -> Result<ParsedEvent, String> {
    let entity = v.get("entity").or_else(|| v.get("data")).unwrap_or(v);
    let id = entity
        .get("id")
        .or_else(|| v.get("id"))
        .and_then(|i| i.as_str())
        .ok_or("missing notion id")?;
    Ok(ParsedEvent::Items(vec![IngestItem {
        provider: "notion".into(),
        key: id.to_string(),
        title: entity
            .get("title")
            .or_else(|| entity.get("name"))
            .and_then(|t| t.as_str())
            .unwrap_or("Notion page")
            .to_string(),
        url: entity
            .get("url")
            .and_then(|u| u.as_str())
            .map(ToOwned::to_owned),
        summary: String::new(),
        closed: false,
    }]))
}

fn parse_bitbucket(v: &Value) -> Result<ParsedEvent, String> {
    let issue = v.get("issue").ok_or("missing issue")?;
    let repo = v
        .pointer("/repository/full_name")
        .and_then(|r| r.as_str())
        .unwrap_or("");
    let id = issue
        .get("id")
        .and_then(|i| {
            i.as_u64()
                .map(|n| n.to_string())
                .or_else(|| i.as_str().map(ToOwned::to_owned))
        })
        .ok_or("missing issue id")?;
    let key = if repo.is_empty() {
        id
    } else {
        format!("{repo}#{id}")
    };
    let state = issue
        .get("state")
        .and_then(|s| s.as_str())
        .unwrap_or("open");
    Ok(ParsedEvent::Items(vec![IngestItem {
        provider: "bitbucket".into(),
        key,
        title: issue
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or("untitled")
            .to_string(),
        url: None,
        summary: issue
            .get("content")
            .and_then(|c| c.get("raw").or(c.as_str().map(|_| c)))
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .chars()
            .take(4000)
            .collect(),
        closed: tickets::status_looks_done(state),
    }]))
}

fn parse_teams(v: &Value) -> Result<ParsedEvent, String> {
    let text = v
        .pointer("/text")
        .or_else(|| v.pointer("/body/content"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .trim();
    let id = v.get("id").and_then(|i| i.as_str()).unwrap_or("");
    if id.is_empty() || text.is_empty() {
        return Ok(ParsedEvent::Ignore);
    }
    let chat_id = v
        .pointer("/conversation/id")
        .or_else(|| v.get("chatId"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    let key = if chat_id.is_empty() {
        id.to_string()
    } else {
        format!("{chat_id}:{id}")
    };
    Ok(ParsedEvent::Items(vec![IngestItem {
        provider: "teams".into(),
        key,
        title: text.chars().take(80).collect(),
        url: None,
        summary: text.chars().take(4000).collect(),
        closed: false,
    }]))
}

fn parse_confluence(v: &Value) -> Result<ParsedEvent, String> {
    let page = v.get("page").unwrap_or(v);
    let id = page
        .get("id")
        .and_then(|i| {
            i.as_str()
                .map(ToOwned::to_owned)
                .or_else(|| i.as_u64().map(|n| n.to_string()))
        })
        .ok_or("missing confluence id")?;
    Ok(ParsedEvent::Items(vec![IngestItem {
        provider: "confluence".into(),
        key: id,
        title: page
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or("page")
            .to_string(),
        url: None,
        summary: String::new(),
        closed: page.get("status").and_then(|s| s.as_str()) == Some("trashed"),
    }]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn parses_github_issue_opened() {
        let mut headers = HeaderMap::new();
        headers.insert("x-github-event", HeaderValue::from_static("issues"));
        let body = json!({
            "action": "opened",
            "issue": { "number": 12, "title": "Bug", "html_url": "https://github.com/acme/app/issues/12", "body": "fix me", "state": "open" },
            "repository": { "full_name": "acme/app" }
        });
        let parsed = parse_event("github", &headers, &serde_json::to_vec(&body).unwrap()).unwrap();
        match parsed {
            ParsedEvent::Items(items) => {
                assert_eq!(items[0].key, "acme/app#12");
                assert_eq!(items[0].title, "Bug");
                assert!(!items[0].closed);
            }
            other => panic!("expected items, got {other:?}"),
        }
    }

    #[test]
    fn github_hmac_accepts_literal_sha256_header() {
        let body = b"{\"zen\":\"hi\"}";
        assert!(github_signature_ok(
            "s3cret",
            "sha256=b18ce78d18646301e6811d72dbafbde385e5ab0f65482fdb261485ca0773c8bc",
            body,
        ));
    }

    #[test]
    fn github_hmac_rejects_bad_sha256_header() {
        let body = b"{\"zen\":\"hi\"}";
        assert!(!github_signature_ok("s3cret", "sha256=deadbeef", body));
    }

    #[test]
    fn slack_url_verification_is_challenge() {
        let bytes =
            serde_json::to_vec(&json!({"type":"url_verification","challenge":"abc"})).unwrap();
        assert_eq!(slack_challenge(&bytes), Some(json!({"challenge":"abc"})));
    }

    #[test]
    fn parses_jira_issue_created() {
        let jira = json!({
            "webhookEvent": "jira:issue_created",
            "issue": { "key": "ENG-9", "fields": { "summary": "Login", "status": { "name": "To Do" } } }
        });
        let parsed = parse_event(
            "jira",
            &HeaderMap::new(),
            &serde_json::to_vec(&jira).unwrap(),
        )
        .unwrap();
        match parsed {
            ParsedEvent::Items(items) => {
                assert_eq!(items[0].provider, "jira");
                assert_eq!(items[0].key, "ENG-9");
                assert_eq!(items[0].title, "Login");
                assert!(!items[0].closed);
            }
            other => panic!("expected items, got {other:?}"),
        }
    }

    #[test]
    fn parses_linear_issue_create() {
        let linear = json!({
            "type": "Issue",
            "action": "create",
            "data": { "identifier": "ENG-3", "title": "Ship", "state": { "type": "unstarted" } }
        });
        let parsed = parse_event(
            "linear",
            &HeaderMap::new(),
            &serde_json::to_vec(&linear).unwrap(),
        )
        .unwrap();
        match parsed {
            ParsedEvent::Items(items) => {
                assert_eq!(items[0].provider, "linear");
                assert_eq!(items[0].key, "ENG-3");
                assert_eq!(items[0].title, "Ship");
                assert!(!items[0].closed);
            }
            other => panic!("expected items, got {other:?}"),
        }
    }

    #[test]
    fn parse_linear_non_issue_without_identifier_is_ignored() {
        let body = json!({
            "type": "Comment",
            "action": "create",
            "data": { "body": "note" }
        });
        let parsed = parse_event(
            "linear",
            &HeaderMap::new(),
            &serde_json::to_vec(&body).unwrap(),
        )
        .unwrap();
        assert!(matches!(parsed, ParsedEvent::Ignore));
    }

    #[test]
    fn parse_jira_done_and_linear_completed_mark_closed() {
        let jira = json!({
            "webhookEvent": "jira:issue_updated",
            "issue": { "key": "ENG-9", "fields": { "summary": "Login", "status": { "name": "Done" } } }
        });
        let parsed = parse_event(
            "jira",
            &HeaderMap::new(),
            &serde_json::to_vec(&jira).unwrap(),
        )
        .unwrap();
        match parsed {
            ParsedEvent::Items(items) => assert!(items[0].closed),
            other => panic!("expected items, got {other:?}"),
        }
        let linear_completed = json!({
            "type": "Issue",
            "action": "update",
            "data": { "identifier": "ENG-3", "title": "Ship", "state": { "type": "completed" } }
        });
        let parsed = parse_event(
            "linear",
            &HeaderMap::new(),
            &serde_json::to_vec(&linear_completed).unwrap(),
        )
        .unwrap();
        match parsed {
            ParsedEvent::Items(items) => assert!(items[0].closed),
            other => panic!("expected items, got {other:?}"),
        }
        let linear_canceled = json!({
            "type": "Issue",
            "action": "update",
            "data": { "identifier": "ENG-3", "title": "Ship", "state": { "type": "canceled" } }
        });
        let parsed = parse_event(
            "linear",
            &HeaderMap::new(),
            &serde_json::to_vec(&linear_canceled).unwrap(),
        )
        .unwrap();
        match parsed {
            ParsedEvent::Items(items) => assert!(items[0].closed),
            other => panic!("expected items, got {other:?}"),
        }
    }

    #[test]
    fn parse_jira_missing_issue_or_key_is_err() {
        let missing_issue = json!({ "webhookEvent": "jira:issue_created" });
        let err = parse_event(
            "jira",
            &HeaderMap::new(),
            &serde_json::to_vec(&missing_issue).unwrap(),
        )
        .unwrap_err();
        assert_eq!(err, "missing issue");
        let missing_key = json!({
            "webhookEvent": "jira:issue_created",
            "issue": { "fields": { "summary": "Login" } }
        });
        let err = parse_event(
            "jira",
            &HeaderMap::new(),
            &serde_json::to_vec(&missing_key).unwrap(),
        )
        .unwrap_err();
        assert_eq!(err, "missing issue.key");
    }

    #[test]
    fn github_issue_comment_is_not_parsed_as_issue_open() {
        let mut headers = HeaderMap::new();
        headers.insert("x-github-event", HeaderValue::from_static("issue_comment"));
        let body = json!({
            "action": "created",
            "issue": { "number": 12, "title": "Bug", "html_url": "https://github.com/acme/app/issues/12", "state": "open" },
            "comment": { "body": "please look" },
            "repository": { "full_name": "acme/app" }
        });
        let parsed = parse_event("github", &headers, &serde_json::to_vec(&body).unwrap()).unwrap();
        match parsed {
            ParsedEvent::Items(items) => {
                assert_eq!(items[0].key, "acme/app#12");
                assert_eq!(items[0].summary, "please look");
            }
            other => panic!("expected comment items, got {other:?}"),
        }
    }

    #[test]
    fn slack_channel_join_is_ignored() {
        let body = json!({
            "event": {
                "type": "message",
                "subtype": "channel_join",
                "channel": "C1",
                "ts": "1.1",
                "text": "<@U1> has joined the channel"
            }
        });
        let parsed = parse_event(
            "slack",
            &HeaderMap::new(),
            &serde_json::to_vec(&body).unwrap(),
        )
        .unwrap();
        assert!(matches!(parsed, ParsedEvent::Ignore));
    }

    #[test]
    fn slack_user_message_ingests_channel_ts() {
        let body = json!({
            "event": {
                "type": "message",
                "channel": "C1",
                "ts": "2.2",
                "text": "please look at weather-app"
            }
        });
        let parsed = parse_event(
            "slack",
            &HeaderMap::new(),
            &serde_json::to_vec(&body).unwrap(),
        )
        .unwrap();
        match parsed {
            ParsedEvent::Items(items) => {
                assert_eq!(items[0].provider, "slack");
                assert_eq!(items[0].key, "C1:2.2");
                assert_eq!(items[0].title, "please look at weather-app");
            }
            other => panic!("expected items, got {other:?}"),
        }
    }

    #[test]
    fn slack_rejects_stale_timestamp() {
        let mut headers = HeaderMap::new();
        headers.insert("x-slack-request-timestamp", HeaderValue::from_static("1"));
        assert!(!slack_timestamp_fresh_at(&headers, 1_000_000));
        headers.insert(
            "x-slack-request-timestamp",
            HeaderValue::from_static("1000000"),
        );
        assert!(slack_timestamp_fresh_at(&headers, 1_000_000));
        assert!(slack_timestamp_fresh_at(&headers, 1_000_280));
        assert!(!slack_timestamp_fresh_at(&headers, 1_000_301));
    }

    #[test]
    fn gitea_issue_event_ingests_repo_key() {
        let mut headers = HeaderMap::new();
        headers.insert("x-gitea-event", HeaderValue::from_static("issues"));
        let body = json!({
            "repository": { "full_name": "worms/game" },
            "issue": { "number": 1, "title": "Add a worm", "state": "open", "body": "grid" }
        });
        let parsed = parse_event("gitea", &headers, &serde_json::to_vec(&body).unwrap()).unwrap();
        match parsed {
            ParsedEvent::Items(items) => {
                assert_eq!(items[0].provider, "gitea");
                assert_eq!(items[0].key, "worms/game#1");
                assert_eq!(items[0].title, "Add a worm");
                assert!(!items[0].closed);
            }
            other => panic!("expected items, got {other:?}"),
        }
    }

    #[test]
    fn gitlab_issue_with_project_and_kind_ingests_key() {
        let body = json!({
            "object_kind": "issue",
            "project": { "path_with_namespace": "acme/app" },
            "object_attributes": { "iid": 3, "title": "x" }
        });
        let parsed = parse_event(
            "gitlab",
            &HeaderMap::new(),
            &serde_json::to_vec(&body).unwrap(),
        )
        .unwrap();
        match parsed {
            ParsedEvent::Items(items) => {
                assert_eq!(items[0].key, "acme/app#3");
                assert_eq!(items[0].title, "x");
                assert!(!items[0].closed);
            }
            other => panic!("expected items, got {other:?}"),
        }
    }

    #[test]
    fn gitlab_issue_kind_without_project_is_ignored() {
        let body = json!({
            "object_kind": "issue",
            "object_attributes": { "iid": 3, "title": "x" }
        });
        let parsed = parse_event(
            "gitlab",
            &HeaderMap::new(),
            &serde_json::to_vec(&body).unwrap(),
        )
        .unwrap();
        assert!(matches!(parsed, ParsedEvent::Ignore));
    }

    #[test]
    fn gitlab_unknown_kind_with_project_is_ignored() {
        let body = json!({
            "object_kind": "push",
            "project": { "path_with_namespace": "acme/app" },
            "object_attributes": { "iid": 3, "title": "x" }
        });
        let parsed = parse_event(
            "gitlab",
            &HeaderMap::new(),
            &serde_json::to_vec(&body).unwrap(),
        )
        .unwrap();
        assert!(matches!(parsed, ParsedEvent::Ignore));
    }
}
