use axum::http::header::{HeaderMap, HeaderValue, SET_COOKIE};
use cookie::{Cookie, SameSite};
use time::Duration as TimeDuration;

use crate::config::AuthConfig;
use crate::tokens::{ACCESS_COOKIE, REFRESH_COOKIE};

fn base_cookie<'a>(name: &'a str, value: &'a str, cfg: &AuthConfig) -> Cookie<'a> {
    let mut c = Cookie::new(name, value);
    c.set_http_only(true);
    c.set_path("/");
    c.set_secure(cfg.cookie_secure);
    c.set_same_site(cfg.cookie_samesite);
    c
}

pub fn set_auth_cookies(headers: &mut HeaderMap, cfg: &AuthConfig, access: &str, refresh: &str) {
    let mut access_c = base_cookie(ACCESS_COOKIE, access, cfg);
    access_c.set_max_age(TimeDuration::seconds(cfg.access_ttl.as_secs() as i64));

    let mut refresh_c = base_cookie(REFRESH_COOKIE, refresh, cfg);
    refresh_c.set_max_age(TimeDuration::seconds(cfg.refresh_ttl.as_secs() as i64));

    append_set_cookie(headers, &access_c);
    append_set_cookie(headers, &refresh_c);
}

pub fn clear_auth_cookies(headers: &mut HeaderMap, cfg: &AuthConfig) {
    let mut access_c = base_cookie(ACCESS_COOKIE, "", cfg);
    access_c.set_max_age(TimeDuration::seconds(0));
    let mut refresh_c = base_cookie(REFRESH_COOKIE, "", cfg);
    refresh_c.set_max_age(TimeDuration::seconds(0));
    append_set_cookie(headers, &access_c);
    append_set_cookie(headers, &refresh_c);
}

fn append_set_cookie(headers: &mut HeaderMap, cookie: &Cookie<'_>) {
    // cookie crate Display produces a valid Set-Cookie value
    if let Ok(val) = HeaderValue::from_str(&cookie.to_string()) {
        headers.append(SET_COOKIE, val);
    }
}

/// Prefer explicit SameSite::None only when secure (browsers require it).
pub fn validate_cookie_policy(cfg: &AuthConfig) {
    if matches!(cfg.cookie_samesite, SameSite::None) && !cfg.cookie_secure {
        tracing::warn!(
            "COOKIE_SAMESITE=None requires COOKIE_SECURE=true in modern browsers"
        );
    }
}
