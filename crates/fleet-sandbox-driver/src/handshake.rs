//! HMAC-SHA256 handshake for volume request files.
//!
//! Compose injects `FLEET_HANDSHAKE_SECRET` into the driver at start. We copy it
//! into process memory and unset the env var before any harness spawn. On Linux
//! we also `PR_SET_DUMPABLE=0` so a same-uid child cannot read `/proc/1/environ`
//! (that file still shows the *initial* execve environment).

use anyhow::{bail, Context, Result};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use std::sync::Mutex;

type HmacSha256 = Hmac<Sha256>;

static SECRET: Mutex<Option<String>> = Mutex::new(None);

fn stored_secret() -> Option<String> {
    SECRET
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .filter(|s| !s.is_empty())
}

/// Load secret from env, drop it from the process environ, lock down `/proc`.
pub fn init_from_env() {
    if let Some(s) = std::env::var("FLEET_HANDSHAKE_SECRET")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        if let Ok(mut g) = SECRET.lock() {
            *g = Some(s);
        }
    }
    // SAFETY: called once at process start, before any threads spawn the harness.
    unsafe {
        std::env::remove_var("FLEET_HANDSHAKE_SECRET");
    }
    lock_down_proc();
}

#[cfg(target_os = "linux")]
fn lock_down_proc() {
    // Same-uid children (ACP harness) must not read /proc/<driver>/environ.
    let _ = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) };
}

#[cfg(not(target_os = "linux"))]
fn lock_down_proc() {}

pub fn handshake_secret() -> Option<String> {
    // Tests may still set the env var; production uses the in-memory copy.
    std::env::var("FLEET_HANDSHAKE_SECRET")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(stored_secret)
}

/// Canonical JSON matching Python `json.dumps(..., sort_keys=True, separators=(",", ":"))`.
pub fn canonical_json(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => {
            if *b {
                "true".into()
            } else {
                "false".into()
            }
        }
        Value::Number(n) => n.to_string(),
        Value::String(s) => serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into()),
        Value::Array(arr) => {
            let inner: Vec<String> = arr.iter().map(canonical_json).collect();
            format!("[{}]", inner.join(","))
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let inner: Vec<String> = keys
                .into_iter()
                .map(|k| {
                    let key = serde_json::to_string(k).unwrap_or_else(|_| "\"\"".into());
                    format!("{}:{}", key, canonical_json(&map[k]))
                })
                .collect();
            format!("{{{}}}", inner.join(","))
        }
    }
}

pub fn sign_value(mut body: Value) -> Result<Value> {
    let secret = handshake_secret().context("FLEET_HANDSHAKE_SECRET is required to sign volume requests")?;
    if let Value::Object(ref mut map) = body {
        map.remove("hmac");
    }
    let raw = canonical_json(&body);
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).context("hmac key")?;
    mac.update(raw.as_bytes());
    let hex = hex::encode(mac.finalize().into_bytes());
    if let Value::Object(ref mut map) = body {
        map.insert("hmac".into(), Value::String(hex));
    }
    Ok(body)
}

pub fn verify_value(body: &Value) -> Result<()> {
    let secret = handshake_secret().context("FLEET_HANDSHAKE_SECRET is not set; refusing volume request")?;
    let got = body
        .get("hmac")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let mut unsigned = body.clone();
    if let Value::Object(ref mut map) = unsigned {
        map.remove("hmac");
    }
    let raw = canonical_json(&unsigned);
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).context("hmac key")?;
    mac.update(raw.as_bytes());
    let expect = hex::encode(mac.finalize().into_bytes());
    if got.len() != expect.len() || !constant_time_eq(got.as_bytes(), expect.as_bytes()) {
        bail!("invalid handshake hmac");
    }
    Ok(())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex as TestMutex;

    static TEST_ENV_LOCK: TestMutex<()> = TestMutex::new(());

    #[test]
    fn canonical_matches_python_style() {
        let v = json!({"b": 1, "a": {"z": true, "y": [1, "x"]}});
        assert_eq!(
            canonical_json(&v),
            r#"{"a":{"y":[1,"x"],"z":true},"b":1}"#
        );
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("FLEET_HANDSHAKE_SECRET", "unit-test-secret");
        }
        let signed = sign_value(json!({
            "action": "provision",
            "job_id": "j1",
            "req_id": "r1"
        }))
        .unwrap();
        verify_value(&signed).unwrap();
        let mut bad = signed.clone();
        bad["hmac"] = json!("00");
        assert!(verify_value(&bad).is_err());
        unsafe {
            std::env::remove_var("FLEET_HANDSHAKE_SECRET");
        }
    }

    #[test]
    fn init_from_env_strips_environ_keeps_secret() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let prev = stored_secret();
        unsafe {
            std::env::set_var("FLEET_HANDSHAKE_SECRET", "init-secret");
        }
        init_from_env();
        assert!(
            std::env::var("FLEET_HANDSHAKE_SECRET").is_err(),
            "secret must not remain in the process environ"
        );
        assert_eq!(stored_secret().as_deref(), Some("init-secret"));
        assert_eq!(handshake_secret().as_deref(), Some("init-secret"));
        let signed = sign_value(json!({"action": "provision", "job_id": "j1"})).unwrap();
        verify_value(&signed).unwrap();
        if let Ok(mut g) = SECRET.lock() {
            *g = prev;
        }
        unsafe {
            std::env::remove_var("FLEET_HANDSHAKE_SECRET");
        }
    }
}
