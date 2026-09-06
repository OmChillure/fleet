//! Per-IP sliding windows for signup/login.
//!
//! Prefer `X-Real-IP` (last proxy `$remote_addr`), then the right-most valid
//! `X-Forwarded-For` hop, then the connecting socket. Skip only when none of
//! those exist (in-process oneshot tests with no proxy headers).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::HeaderMap;
use axum::http::request::Parts;

use crate::error::{AppError, AppResult};

/// Connecting socket when the server was started with
/// `into_make_service_with_connect_info`. Missing in in-process oneshot tests.
pub struct ClientAddr(pub Option<SocketAddr>);

impl<S> FromRequestParts<S> for ClientAddr
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        let addr = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(a)| *a);
        std::future::ready(Ok(Self(addr)))
    }
}

struct Limiter {
    windows: Mutex<HashMap<(String, String), Vec<Instant>>>,
}

fn limiter() -> &'static Limiter {
    static LIMITER: OnceLock<Limiter> = OnceLock::new();
    LIMITER.get_or_init(|| Limiter {
        windows: Mutex::new(HashMap::new()),
    })
}

fn parse_ip_token(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    t.parse::<std::net::IpAddr>().ok().map(|ip| ip.to_string())
}

fn header_ip(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_ip_token)
}

fn xff_rightmost(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').rev().find_map(parse_ip_token))
}

pub fn client_ip(headers: &HeaderMap, connect: Option<SocketAddr>) -> Option<String> {
    header_ip(headers, "x-real-ip")
        .or_else(|| xff_rightmost(headers))
        .or_else(|| connect.map(|a| a.ip().to_string()))
}

/// Positive `AUTH_*_RATE_MAX` env, else `default`. Zero/invalid keep the default.
pub fn max_from_env(var: &str, default: usize) -> usize {
    std::env::var(var)
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}

/// Positive `AUTH_*_RATE_WINDOW_SECS` env, else `default_secs`.
pub fn window_from_env(var: &str, default_secs: u64) -> Duration {
    let secs = std::env::var(var)
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default_secs);
    Duration::from_secs(secs)
}

pub fn check(
    action: &str,
    headers: &HeaderMap,
    connect: Option<SocketAddr>,
    max: usize,
    window: Duration,
) -> AppResult<()> {
    let Some(ip) = client_ip(headers, connect) else {
        return Ok(());
    };
    let now = Instant::now();
    let mut map = limiter()
        .windows
        .lock()
        .map_err(|_| AppError::internal("rate limiter"))?;
    let key = (action.to_string(), ip);
    {
        let hits = map.entry(key.clone()).or_default();
        hits.retain(|t| now.duration_since(*t) < window);
        if hits.len() >= max {
            return Err(AppError::too_many_requests(
                "too many attempts — try again later",
            ));
        }
        hits.push(now);
    }
    map.retain(|_, hits| {
        hits.retain(|t| now.duration_since(*t) < window);
        !hits.is_empty()
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderName, HeaderValue};

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                HeaderName::from_bytes(k.as_bytes()).expect("header name"),
                HeaderValue::from_str(v).expect("header value"),
            );
        }
        h
    }

    fn window_len(action: &str, ip: &str) -> usize {
        limiter()
            .windows
            .lock()
            .expect("limiter")
            .get(&(action.to_string(), ip.to_string()))
            .map(|v| v.len())
            .unwrap_or(0)
    }

    #[test]
    fn xff_uses_rightmost_valid_hop() {
        let h = headers(&[("x-forwarded-for", "1.2.3.4, 198.51.100.20")]);
        assert_eq!(client_ip(&h, None).as_deref(), Some("198.51.100.20"));
    }

    #[test]
    fn connect_info_used_when_proxy_headers_absent() {
        let addr: SocketAddr = "198.51.100.9:443".parse().unwrap();
        assert_eq!(
            client_ip(&HeaderMap::new(), Some(addr)).as_deref(),
            Some("198.51.100.9")
        );
        assert!(client_ip(&HeaderMap::new(), None).is_none());
    }

    #[test]
    fn evicts_empty_windows() {
        let action = "evict-empty-window";
        let ip = "203.0.113.50";
        let h = headers(&[("x-real-ip", ip)]);
        let window = Duration::from_millis(250);
        check(action, &h, None, 2, window).unwrap();
        check(action, &h, None, 2, window).unwrap();
        let err = check(action, &h, None, 2, window).expect_err("third hit at max=2");
        assert!(
            matches!(err, AppError::TooManyRequests(_)),
            "expected too_many_requests at max=2, got {err:?}"
        );
        std::thread::sleep(Duration::from_millis(300));
        check(action, &h, None, 2, window).unwrap();
    }

    #[test]
    fn check_drops_expired_foreign_key() {
        let action = "drop-expired-foreign";
        let ip_a = "203.0.113.52";
        let ip_b = "203.0.113.53";
        let ha = headers(&[("x-real-ip", ip_a)]);
        let hb = headers(&[("x-real-ip", ip_b)]);
        let window = Duration::from_millis(1);
        check(action, &ha, None, 5, window).unwrap();
        assert_eq!(window_len(action, ip_a), 1);
        std::thread::sleep(Duration::from_millis(5));
        check(action, &hb, None, 5, window).unwrap();
        assert_eq!(
            window_len(action, ip_a),
            0,
            "expired A must be retain-dropped when B is checked"
        );
        assert_eq!(window_len(action, ip_b), 1);
    }

    #[test]
    fn max_from_env_invalid_or_zero_keeps_default() {
        assert_eq!(max_from_env("AUTH_RATE_MAX_UNSET_FOR_TEST", 5), 5);
        assert_eq!(
            window_from_env("AUTH_RATE_WINDOW_UNSET_FOR_TEST", 900),
            Duration::from_secs(900)
        );
    }

    #[test]
    fn check_skips_when_client_ip_absent() {
        let action = "skip-absent-client-ip";
        check(action, &HeaderMap::new(), None, 0, Duration::from_secs(60)).unwrap();
        assert!(
            limiter()
                .windows
                .lock()
                .expect("limiter")
                .keys()
                .all(|(a, _)| a != action),
            "absent client_ip must not insert a limiter key"
        );
    }
}
