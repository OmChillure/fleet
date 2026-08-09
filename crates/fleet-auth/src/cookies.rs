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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use axum::http::header::SET_COOKIE;

    fn test_cfg(cookie_secure: bool) -> AuthConfig {
        AuthConfig {
            jwt_secret: "test-secret-at-least-32-characters-long!!".into(),
            access_ttl: Duration::from_secs(900),
            refresh_ttl: Duration::from_secs(604_800),
            cookie_secure,
            cookie_samesite: SameSite::Lax,
        }
    }

    fn set_cookie_values(headers: &HeaderMap) -> Vec<String> {
        headers
            .get_all(SET_COOKIE)
            .iter()
            .map(|v| v.to_str().expect("set-cookie utf8").to_string())
            .collect()
    }

    #[test]
    fn set_auth_cookies_http_only_path_and_secure_follows_config() {
        let cfg = test_cfg(false);
        let mut headers = HeaderMap::new();
        set_auth_cookies(&mut headers, &cfg, "access-token", "refresh-token");

        let cookies = set_cookie_values(&headers);
        assert_eq!(cookies.len(), 2, "expected access + refresh cookies");

        for raw in &cookies {
            let lower = raw.to_ascii_lowercase();
            assert!(
                lower.contains("httponly"),
                "missing HttpOnly: {raw}"
            );
            assert!(
                lower.contains("path=/"),
                "missing Path=/: {raw}"
            );
            assert!(
                !lower.contains("secure"),
                "Secure must follow config=false: {raw}"
            );
        }

        assert!(
            cookies.iter().any(|c| c.starts_with(&format!("{ACCESS_COOKIE}="))),
            "missing access cookie: {cookies:?}"
        );
        assert!(
            cookies
                .iter()
                .any(|c| c.starts_with(&format!("{REFRESH_COOKIE}="))),
            "missing refresh cookie: {cookies:?}"
        );
    }

    #[test]
    fn clear_auth_cookies_max_age_zero() {
        let cfg = test_cfg(false);
        let mut headers = HeaderMap::new();
        clear_auth_cookies(&mut headers, &cfg);

        let cookies = set_cookie_values(&headers);
        assert_eq!(cookies.len(), 2, "expected access + refresh clear cookies");

        for raw in &cookies {
            let lower = raw.to_ascii_lowercase();
            assert!(
                lower.contains("max-age=0"),
                "clear cookie must set Max-Age=0: {raw}"
            );
            assert!(
                lower.contains("httponly"),
                "missing HttpOnly: {raw}"
            );
            assert!(
                lower.contains("path=/"),
                "missing Path=/: {raw}"
            );
        }
    }
}
