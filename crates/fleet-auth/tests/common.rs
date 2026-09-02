//! Shared in-process Axum harness for fleet-auth HTTP tests.
//! Fail-closed: missing DATABASE_URL panics (never skip).

use std::collections::BTreeMap;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use fleet_auth::{ACCESS_COOKIE, AuthConfig, AuthState, REFRESH_COOKIE};
use fleet_db::{PoolConfig, connect_and_migrate_with};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

pub const TEST_PASSWORD: &str = "correct-horse";

pub fn require_database_url() -> String {
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

pub fn unique_email() -> String {
    let uuid = Uuid::new_v4();
    format!("auth.{uuid}@example.test")
}

fn test_auth_config() -> AuthConfig {
    let jwt_secret = std::env::var("JWT_SECRET")
        .ok()
        .filter(|s| s.len() >= 32)
        .unwrap_or_else(|| "dev-change-me-to-a-long-random-secret-at-least-32-chars".into());
    AuthConfig {
        jwt_secret,
        access_ttl: Duration::from_secs(900),
        refresh_ttl: Duration::from_secs(604_800),
        cookie_secure: false,
        cookie_samesite: cookie::SameSite::Lax,
        signup_token: None,
    }
}

pub async fn connect_and_migrate() -> fleet_db::PgPool {
    let url = require_database_url();
    connect_and_migrate_with(
        &url,
        &PoolConfig {
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

pub async fn app() -> axum::Router {
    app_with_config(test_auth_config()).await
}

pub async fn app_with_signup_token(token: &str) -> axum::Router {
    let mut config = test_auth_config();
    config.signup_token = Some(token.to_string());
    app_with_config(config).await
}

async fn app_with_config(config: AuthConfig) -> axum::Router {
    let db = connect_and_migrate().await;
    let state = AuthState { db, config };
    fleet_auth::router().with_state(state)
}

/// Parsed `Set-Cookie` name=value plus the raw header (for HttpOnly checks).
#[derive(Debug, Clone)]
pub struct ParsedSetCookie {
    pub name: String,
    pub value: String,
    pub raw: String,
}

impl ParsedSetCookie {
    pub fn is_httponly(&self) -> bool {
        self.raw
            .split(';')
            .any(|part| part.trim().eq_ignore_ascii_case("HttpOnly"))
    }

    pub fn path(&self) -> Option<&str> {
        self.raw.split(';').find_map(|part| {
            let p = part.trim();
            p.split_once('=')
                .filter(|(k, _)| k.eq_ignore_ascii_case("Path"))
                .map(|(_, v)| v)
        })
    }

    pub fn max_age(&self) -> Option<i64> {
        self.raw.split(';').find_map(|part| {
            let p = part.trim();
            p.split_once('=')
                .filter(|(k, _)| k.eq_ignore_ascii_case("Max-Age"))
                .and_then(|(_, v)| v.parse().ok())
        })
    }

    pub fn is_secure(&self) -> bool {
        self.raw
            .split(';')
            .any(|part| part.trim().eq_ignore_ascii_case("Secure"))
    }
}

pub fn parse_set_cookies(headers: &HeaderMap) -> Vec<ParsedSetCookie> {
    headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|raw| {
            let nv = raw.split(';').next().unwrap_or(raw);
            let (name, value) = nv.split_once('=')?;
            Some(ParsedSetCookie {
                name: name.trim().to_string(),
                value: value.trim().to_string(),
                raw: raw.to_string(),
            })
        })
        .collect()
}

/// Client-side cookie jar for `fleet_access` / `fleet_refresh`.
#[derive(Debug, Clone, Default)]
pub struct CookieJar {
    values: BTreeMap<String, String>,
}

impl CookieJar {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    pub fn access(&self) -> Option<&str> {
        self.get(ACCESS_COOKIE)
    }

    pub fn refresh(&self) -> Option<&str> {
        self.get(REFRESH_COOKIE)
    }

    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.values.insert(name.into(), value.into());
    }

    pub fn absorb(&mut self, headers: &HeaderMap) {
        for c in parse_set_cookies(headers) {
            if c.value.is_empty() {
                self.values.remove(&c.name);
            } else {
                self.values.insert(c.name, c.value);
            }
        }
    }

    pub fn cookie_header(&self) -> Option<String> {
        if self.values.is_empty() {
            return None;
        }
        Some(
            self.values
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }
}

#[derive(Debug)]
pub struct HttpOut {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Value,
}

impl HttpOut {
    pub fn set_cookies(&self) -> Vec<ParsedSetCookie> {
        parse_set_cookies(&self.headers)
    }

    pub fn cookie(&self, name: &str) -> Option<ParsedSetCookie> {
        self.set_cookies().into_iter().find(|c| c.name == name)
    }

    pub fn error(&self) -> &str {
        self.body["error"].as_str().unwrap_or("")
    }

    pub fn absorb_into(&self, jar: &mut CookieJar) {
        jar.absorb(&self.headers);
    }
}

pub async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    cookies: Option<&CookieJar>,
    body: Option<Value>,
) -> HttpOut {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    if let Some(jar) = cookies {
        if let Some(h) = jar.cookie_header() {
            builder = builder.header(header::COOKIE, h);
        }
    }
    call_finish(app, builder, body).await
}

pub async fn call_with_xff(
    app: &axum::Router,
    method: &str,
    uri: &str,
    xff: &str,
    body: Option<Value>,
) -> HttpOut {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    builder = builder.header("x-forwarded-for", xff);
    call_finish(app, builder, body).await
}

pub async fn call_with_proxy_ips(
    app: &axum::Router,
    method: &str,
    uri: &str,
    xff: &str,
    real_ip: &str,
    body: Option<Value>,
) -> HttpOut {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    builder = builder.header("x-forwarded-for", xff);
    builder = builder.header("x-real-ip", real_ip);
    call_finish(app, builder, body).await
}

async fn call_finish(
    app: &axum::Router,
    builder: axum::http::request::Builder,
    body: Option<Value>,
) -> HttpOut {
    let req = builder
        .body(match body {
            Some(b) => Body::from(serde_json::to_vec(&b).unwrap()),
            None => Body::empty(),
        })
        .unwrap();
    let res = app.clone().oneshot(req).await.expect("oneshot");
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({ "raw": String::from_utf8_lossy(&bytes) }))
    };
    HttpOut {
        status,
        headers,
        body,
    }
}

pub async fn signup(app: &axum::Router, email: &str, password: &str) -> HttpOut {
    signup_with(app, email, password, None).await
}

pub async fn signup_with(
    app: &axum::Router,
    email: &str,
    password: &str,
    display_name: Option<&str>,
) -> HttpOut {
    let mut body = json!({ "email": email, "password": password });
    if let Some(name) = display_name {
        body["display_name"] = json!(name);
    }
    call(app, "POST", "/api/auth/signup", None, Some(body)).await
}

pub async fn signup_with_token(
    app: &axum::Router,
    email: &str,
    password: &str,
    token: Option<&str>,
) -> HttpOut {
    let mut body = json!({ "email": email, "password": password });
    if let Some(token) = token {
        body["token"] = json!(token);
    }
    call(app, "POST", "/api/auth/signup", None, Some(body)).await
}

pub async fn login(app: &axum::Router, email: &str, password: &str) -> (HttpOut, CookieJar) {
    let out = call(
        app,
        "POST",
        "/api/auth/login",
        None,
        Some(json!({ "email": email, "password": password })),
    )
    .await;
    let mut jar = CookieJar::default();
    out.absorb_into(&mut jar);
    (out, jar)
}

pub async fn signup_and_login(app: &axum::Router) -> (String, CookieJar, HttpOut) {
    let email = unique_email();
    let created = signup(app, &email, TEST_PASSWORD).await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "signup: {}",
        created.body
    );
    let (login_out, jar) = login(app, &email, TEST_PASSWORD).await;
    assert_eq!(
        login_out.status,
        StatusCode::OK,
        "login: {}",
        login_out.body
    );
    (email, jar, login_out)
}
