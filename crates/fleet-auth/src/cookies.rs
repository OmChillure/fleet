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
    // cookie crate Display produces a valid Set-Cookie value. Dropping it
    // would make login look successful with no session cookie.
    let raw = cookie.to_string();
    let val = HeaderValue::from_str(&raw).unwrap_or_else(|e| {
        panic!("Set-Cookie header is invalid ({e}): {raw}");
    });
    headers.append(SET_COOKIE, val);
}

/// Prefer explicit SameSite::None only when secure (browsers require it).
pub fn validate_cookie_policy(cfg: &AuthConfig) {
    if matches!(cfg.cookie_samesite, SameSite::None) && !cfg.cookie_secure {
        tracing::warn!("COOKIE_SAMESITE=None requires COOKIE_SECURE=true in modern browsers");
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
            signup_token: None,
            instance_admin_emails: Vec::new(),
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
        for cookie_secure in [false, true] {
            let cfg = test_cfg(cookie_secure);
            let mut headers = HeaderMap::new();
            set_auth_cookies(&mut headers, &cfg, "access-token", "refresh-token");

            let cookies = set_cookie_values(&headers);
            assert_eq!(cookies.len(), 2, "expected access + refresh cookies");

            for raw in &cookies {
                let has_secure = raw
                    .split(';')
                    .any(|part| part.trim().eq_ignore_ascii_case("secure"));
                assert_eq!(
                    has_secure,
                    cookie_secure,
                    "Secure must follow cookie_secure={cookie_secure}: {raw}"
                );
            }
        }
    }

    #[test]
    fn set_auth_cookies_max_age_follows_access_and_refresh_ttl() {
        let cfg = test_cfg(false);
        let mut headers = HeaderMap::new();
        set_auth_cookies(&mut headers, &cfg, "access-token", "refresh-token");

        let cookies = set_cookie_values(&headers);
        let access = cookies
            .iter()
            .find(|c| c.starts_with(&format!("{ACCESS_COOKIE}=")))
            .unwrap_or_else(|| panic!("missing access cookie: {cookies:?}"));
        let refresh = cookies
            .iter()
            .find(|c| c.starts_with(&format!("{REFRESH_COOKIE}=")))
            .unwrap_or_else(|| panic!("missing refresh cookie: {cookies:?}"));

        assert!(
            access
                .split(';')
                .any(|part| part.trim().eq_ignore_ascii_case("Max-Age=900")),
            "access Max-Age must be access_ttl 900s: {access}"
        );
        assert!(
            refresh
                .split(';')
                .any(|part| part.trim().eq_ignore_ascii_case("Max-Age=604800")),
            "refresh Max-Age must be refresh_ttl 604800s: {refresh}"
        );
    }

    #[test]
    fn set_auth_cookies_samesite_follows_config() {
        for samesite in [SameSite::Lax, SameSite::Strict, SameSite::None] {
            let mut cfg = test_cfg(true);
            cfg.cookie_samesite = samesite;
            let mut headers = HeaderMap::new();
            set_auth_cookies(&mut headers, &cfg, "access-token", "refresh-token");

            let cookies = set_cookie_values(&headers);
            assert_eq!(cookies.len(), 2, "expected access + refresh cookies");

            let expected = format!("SameSite={samesite}");
            for raw in &cookies {
                assert!(
                    raw.split(';')
                        .any(|part| part.trim().eq_ignore_ascii_case(&expected)),
                    "SameSite must follow config={samesite}: {raw}"
                );
            }
        }
    }

    #[test]
    fn set_auth_cookies_writes_provided_token_values() {
        let cfg = test_cfg(false);
        let mut headers = HeaderMap::new();
        set_auth_cookies(&mut headers, &cfg, "access-token", "refresh-token");

        let cookies = set_cookie_values(&headers);
        assert!(
            cookies
                .iter()
                .any(|c| c.starts_with(&format!("{ACCESS_COOKIE}=access-token"))),
            "missing {ACCESS_COOKIE}=access-token: {cookies:?}"
        );
        assert!(
            cookies
                .iter()
                .any(|c| c.starts_with(&format!("{REFRESH_COOKIE}=refresh-token"))),
            "missing {REFRESH_COOKIE}=refresh-token: {cookies:?}"
        );
    }

    #[test]
    fn clear_auth_cookies_max_age_zero() {
        let cfg = test_cfg(false);
        let mut headers = HeaderMap::new();
        clear_auth_cookies(&mut headers, &cfg);

        let cookies = set_cookie_values(&headers);
        let access = cookies
            .iter()
            .find(|c| c.starts_with(&format!("{ACCESS_COOKIE}=")))
            .unwrap_or_else(|| panic!("missing access cookie: {cookies:?}"));
        let refresh = cookies
            .iter()
            .find(|c| c.starts_with(&format!("{REFRESH_COOKIE}=")))
            .unwrap_or_else(|| panic!("missing refresh cookie: {cookies:?}"));

        for (name, raw) in [(ACCESS_COOKIE, access), (REFRESH_COOKIE, refresh)] {
            let parsed = Cookie::parse(raw.as_str())
                .unwrap_or_else(|e| panic!("parse {name} Set-Cookie ({e}): {raw}"));
            assert_eq!(parsed.name(), name);
            assert_eq!(
                parsed.value(),
                "",
                "clear {name} value must be empty: {raw}"
            );
            assert_eq!(
                parsed.max_age(),
                Some(TimeDuration::seconds(0)),
                "clear {name} must set Max-Age=0: {raw}"
            );
            assert_eq!(
                parsed.path(),
                Some("/"),
                "clear {name} must set Path=/: {raw}"
            );
        }
    }

    #[test]
    fn clear_auth_cookies_secure_and_samesite_match_config() {
        let mut cfg = test_cfg(true);
        cfg.cookie_samesite = SameSite::None;
        let mut headers = HeaderMap::new();
        clear_auth_cookies(&mut headers, &cfg);

        let cookies = set_cookie_values(&headers);
        let access = cookies
            .iter()
            .find(|c| c.starts_with(&format!("{ACCESS_COOKIE}=")))
            .unwrap_or_else(|| panic!("missing access cookie: {cookies:?}"));
        let refresh = cookies
            .iter()
            .find(|c| c.starts_with(&format!("{REFRESH_COOKIE}=")))
            .unwrap_or_else(|| panic!("missing refresh cookie: {cookies:?}"));

        for raw in [access, refresh] {
            let has_secure = raw
                .split(';')
                .any(|part| part.trim().eq_ignore_ascii_case("secure"));
            assert!(
                has_secure,
                "clear must emit Secure when cookie_secure=true: {raw}"
            );
            assert!(
                raw.split(';')
                    .any(|part| part.trim().eq_ignore_ascii_case("SameSite=None")),
                "clear must emit SameSite=None when configured: {raw}"
            );
        }
    }
}
