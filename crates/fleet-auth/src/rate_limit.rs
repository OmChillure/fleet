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
    fn prefers_x_real_ip_over_spoofed_xff() {
        let h = headers(&[
            ("x-forwarded-for", "1.2.3.4, 203.0.113.9"),
            ("x-real-ip", "203.0.113.9"),
        ]);
        assert_eq!(client_ip(&h, None).as_deref(), Some("203.0.113.9"));
    }

    #[test]
    fn xff_uses_rightmost_valid_hop() {
        let h = headers(&[("x-forwarded-for", "1.2.3.4, 198.51.100.20")]);
        assert_eq!(client_ip(&h, None).as_deref(), Some("198.51.100.20"));
    }

    #[test]
    fn invalid_xff_token_falls_through_to_x_real_ip() {
        let h = headers(&[
            ("x-forwarded-for", "unknown, 1.2.3.4:80"),
            ("x-real-ip", "203.0.113.7"),
        ]);
        assert_eq!(client_ip(&h, None).as_deref(), Some("203.0.113.7"));
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
        check(action, &h, None, 5, Duration::from_millis(1)).unwrap();
        assert_eq!(window_len(action, ip), 1);
        std::thread::sleep(Duration::from_millis(5));
        check(action, &h, None, 5, Duration::from_millis(1)).unwrap();
        assert_eq!(window_len(action, ip), 1);
    }
}
