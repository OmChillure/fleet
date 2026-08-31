use axum::http::header::{HeaderMap, HeaderValue, SET_COOKIE};
use cookie::{Cookie, SameSite};
use time::Duration as TimeDuration;

use crate::config::AuthConfig;
use crate::tokens::{ACCESS_COOKIE, REFRESH_COOKIE};

fn base_cookie<'a>(name: &'a str, value: &'a str, cfg: &AuthConfig) -> Cookie<'a> {
    let mut c = Cookie::new(name, value);
    c.set_http_only(true);
    c.set_path("/");
    // Browsers reject SameSite=None without Secure — never mint that pair.
    if matches!(cfg.cookie_samesite, SameSite::None) && !cfg.cookie_secure {
        c.set_secure(false);
        c.set_same_site(SameSite::Lax);
    } else {
        c.set_secure(cfg.cookie_secure);
        c.set_same_site(cfg.cookie_samesite);
    }
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
pub fn validate_cookie_policy(cfg: &AuthConfig) -> anyhow::Result<()> {
    cfg.validate_cookie_policy()
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

    fn test_cfg_samesite(cookie_secure: bool, cookie_samesite: SameSite) -> AuthConfig {
        AuthConfig {
            jwt_secret: "test-secret-at-least-32-characters-long!!".into(),
            access_ttl: Duration::from_secs(900),
            refresh_ttl: Duration::from_secs(604_800),
            cookie_secure,
            cookie_samesite,
        }
    }

    fn cookie_attr_eq(raw: &str, name: &str, value: &str) -> bool {
        raw.split(';').any(|part| {
            part.trim()
                .split_once('=')
                .is_some_and(|(k, v)| k.eq_ignore_ascii_case(name) && v.eq_ignore_ascii_case(value))
        })
    }

    fn has_secure(raw: &str) -> bool {
        raw.split(';')
            .any(|part| part.trim().eq_ignore_ascii_case("Secure"))
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
                let lower = raw.to_ascii_lowercase();
                assert!(lower.contains("httponly"), "missing HttpOnly: {raw}");
                assert!(lower.contains("path=/"), "missing Path=/: {raw}");
                assert_eq!(
                    has_secure(raw),
                    cookie_secure,
                    "Secure must follow config={cookie_secure}: {raw}"
                );
            }

            assert!(
                cookies
                    .iter()
                    .any(|c| c.starts_with(&format!("{ACCESS_COOKIE}="))),
                "missing access cookie: {cookies:?}"
            );
            assert!(
                cookies
                    .iter()
                    .any(|c| c.starts_with(&format!("{REFRESH_COOKIE}="))),
                "missing refresh cookie: {cookies:?}"
            );
        }
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
            assert!(lower.contains("httponly"), "missing HttpOnly: {raw}");
            assert!(lower.contains("path=/"), "missing Path=/: {raw}");
        }
    }

    #[test]
    fn samesite_none_without_secure_set_cookie_is_never_insecure() {
        let cfg = test_cfg_samesite(false, SameSite::None);
        let mut headers = HeaderMap::new();
        set_auth_cookies(&mut headers, &cfg, "access-token", "refresh-token");

        let cookies = set_cookie_values(&headers);
        assert_eq!(cookies.len(), 2, "expected access + refresh cookies");
        for raw in &cookies {
            assert!(
                !cookie_attr_eq(raw, "SameSite", "None") || has_secure(raw),
                "Set-Cookie must never be SameSite=None without Secure: {raw}"
            );
        }
        assert!(
            cookies
                .iter()
                .any(|c| c.starts_with(&format!("{ACCESS_COOKIE}="))),
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
    fn validate_cookie_policy_fails_samesite_none_without_secure() {
        let cfg = test_cfg_samesite(false, SameSite::None);
        let err = validate_cookie_policy(&cfg)
            .expect_err("in-struct None+insecure must fail validate_cookie_policy");
        let lower = format!("{err:#}").to_ascii_lowercase();
        assert!(
            lower.contains("samesite") && lower.contains("secure"),
            "error must mention SameSite and Secure, got {err:#}"
        );
        // Mint-time still coerces so in-struct tests can construct this pair.
        let mut headers = HeaderMap::new();
        set_auth_cookies(&mut headers, &cfg, "access-token", "refresh-token");
        for raw in set_cookie_values(&headers) {
            assert!(
                !cookie_attr_eq(&raw, "SameSite", "None") || has_secure(&raw),
                "base_cookie must still coerce None+insecure: {raw}"
            );
        }
    }

    #[test]
    fn validate_cookie_policy_ok_for_none_with_secure_and_lax_insecure() {
        validate_cookie_policy(&test_cfg_samesite(true, SameSite::None))
            .expect("None+Secure must be allowed");
        validate_cookie_policy(&test_cfg_samesite(false, SameSite::Lax))
            .expect("Lax+insecure must be allowed");
    }

    #[test]
    fn samesite_none_with_secure_set_cookie_includes_none_and_secure() {
        let cfg = test_cfg_samesite(true, SameSite::None);
        let mut headers = HeaderMap::new();
        set_auth_cookies(&mut headers, &cfg, "access-token", "refresh-token");

        let cookies = set_cookie_values(&headers);
        assert_eq!(cookies.len(), 2, "expected access + refresh cookies");
        for raw in &cookies {
            assert!(
                cookie_attr_eq(raw, "SameSite", "None"),
                "COOKIE_SAMESITE=None + Secure must set SameSite=None: {raw}"
            );
            assert!(
                has_secure(raw),
                "COOKIE_SAMESITE=None + Secure must set Secure: {raw}"
            );
        }
    }
}
