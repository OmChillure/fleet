//! Per-IP sliding windows for signup/login. No-ops when the request has no
//! client IP (in-process tests) so existing suites stay unchanged.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::http::HeaderMap;

use crate::error::{AppError, AppResult};

struct Limiter {
    windows: Mutex<HashMap<(String, String), Vec<Instant>>>,
}

fn limiter() -> &'static Limiter {
    static LIMITER: OnceLock<Limiter> = OnceLock::new();
    LIMITER.get_or_init(|| Limiter {
        windows: Mutex::new(HashMap::new()),
    })
}

pub fn client_ip(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.trim().to_string())
        })
        .filter(|s| s.parse::<std::net::IpAddr>().is_ok())
}

pub fn check(action: &str, headers: &HeaderMap, max: usize, window: Duration) -> AppResult<()> {
    let Some(ip) = client_ip(headers) else {
        return Ok(());
    };
    let now = Instant::now();
    let mut map = limiter()
        .windows
        .lock()
        .map_err(|_| AppError::internal("rate limiter"))?;
    let hits = map.entry((action.to_string(), ip)).or_default();
    hits.retain(|t| now.duration_since(*t) < window);
    if hits.len() >= max {
        return Err(AppError::too_many_requests(
            "too many attempts — try again later",
        ));
    }
    hits.push(now);
    Ok(())
}
