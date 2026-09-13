//! HMAC-SHA256 handshake for volume request files.
//!
//! Compose injects `FLEET_HANDSHAKE_SECRET` into the driver at start. We copy it
//! into process memory and unset the env var before any harness spawn. On Linux
//! we also `PR_SET_DUMPABLE=0` so a same-uid child cannot read `/proc/1/environ`
//! (that file still shows the *initial* execve environment).

use anyhow::{Context, Result, bail};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use std::sync::Mutex;

type HmacSha256 = Hmac<Sha256>;

static SECRET: Mutex<Option<String>> = Mutex::new(None);

fn stored_secret() -> Option<String> {
    SECRET
        .lock()
        .expect("handshake secret mutex poisoned")
        .clone()
        .filter(|s| !s.is_empty())
}

/// Load secret from env, wipe it from the execve environ block, unset, lock `/proc`.
/// Must run from a sync `main` **before** any Tokio worker threads start.
pub fn init_from_env() -> Result<()> {
    if let Some(s) = std::env::var("FLEET_HANDSHAKE_SECRET")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        let mut g = SECRET
            .lock()
            .map_err(|_| anyhow::anyhow!("handshake secret mutex poisoned"))?;
        *g = Some(s);
    }
    // Zero the value in the original environ strings *before* unsetenv.
    // `/proc/self/environ` is the execve block and is not updated by unsetenv.
    scrub_secret_from_environ();
    // SAFETY: caller guarantees no other threads yet (see `main`).
    unsafe {
        std::env::remove_var("FLEET_HANDSHAKE_SECRET");
    }
    lock_down_proc()?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn scrub_secret_from_environ() {
    const PREFIX: &[u8] = b"FLEET_HANDSHAKE_SECRET=";
    unsafe {
        unsafe extern "C" {
            static mut environ: *mut *mut libc::c_char;
        }
        if environ.is_null() {
            return;
        }
        let mut p = environ;
        while !(*p).is_null() {
            let entry = std::slice::from_raw_parts(*p as *const u8, libc::strlen(*p));
            if entry.starts_with(PREFIX) {
                let val = (*p).add(PREFIX.len());
                let n = libc::strlen(val);
                std::ptr::write_bytes(val, b'x', n);
            }
            p = p.add(1);
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn scrub_secret_from_environ() {}

#[cfg(target_os = "linux")]
fn lock_down_proc() -> Result<()> {
    let rc = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) };
    if rc != 0 {
        bail!(
            "prctl(PR_SET_DUMPABLE, 0) failed: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn lock_down_proc() -> Result<()> {
    Ok(())
}

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
    let secret =
        handshake_secret().context("FLEET_HANDSHAKE_SECRET is required to sign volume requests")?;
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
    let secret =
        handshake_secret().context("FLEET_HANDSHAKE_SECRET is not set; refusing volume request")?;
    let got = body.get("hmac").and_then(|v| v.as_str()).unwrap_or("");
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
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex as TestMutex;

    static TEST_ENV_LOCK: TestMutex<()> = TestMutex::new(());

    /// Restores env, in-memory SECRET, and dumpable even if the test panics.
    struct HandshakeEnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev_env: Option<String>,
        prev_stored: Option<String>,
        #[cfg(target_os = "linux")]
        prev_dumpable: i32,
    }

    impl HandshakeEnvGuard {
        fn acquire() -> Self {
            let lock = TEST_ENV_LOCK.lock().expect("TEST_ENV_LOCK");
            let prev_env = std::env::var("FLEET_HANDSHAKE_SECRET").ok();
            let prev_stored = SECRET.lock().ok().and_then(|g| g.clone());
            #[cfg(target_os = "linux")]
            let prev_dumpable = unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) };
            Self {
                _lock: lock,
                prev_env,
                prev_stored,
                #[cfg(target_os = "linux")]
                prev_dumpable,
            }
        }

        fn set_env(&self, value: Option<&str>) {
            // SAFETY: TEST_ENV_LOCK is held for this guard's lifetime.
            unsafe {
                match value {
                    Some(v) => std::env::set_var("FLEET_HANDSHAKE_SECRET", v),
                    None => std::env::remove_var("FLEET_HANDSHAKE_SECRET"),
                }
            }
        }

        fn clear_stored(&self) {
            if let Ok(mut g) = SECRET.lock() {
                *g = None;
            }
        }
    }

    impl Drop for HandshakeEnvGuard {
        fn drop(&mut self) {
            if let Ok(mut g) = SECRET.lock() {
                *g = self.prev_stored.clone();
            }
            // SAFETY: TEST_ENV_LOCK is still held (fields drop after Drop::drop).
            unsafe {
                match &self.prev_env {
                    Some(v) => std::env::set_var("FLEET_HANDSHAKE_SECRET", v),
                    None => std::env::remove_var("FLEET_HANDSHAKE_SECRET"),
                }
            }
            #[cfg(target_os = "linux")]
            if self.prev_dumpable >= 0 {
                unsafe {
                    libc::prctl(
                        libc::PR_SET_DUMPABLE,
                        self.prev_dumpable as libc::c_ulong,
                        0,
                        0,
                        0,
                    );
                }
            }
        }
    }

    #[test]
    fn canonical_matches_python_style() {
        let v = json!({"b": 1, "a": {"z": true, "y": [1, "x"]}});
        assert_eq!(canonical_json(&v), r#"{"a":{"y":[1,"x"],"z":true},"b":1}"#);
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
        let hmac = signed["hmac"].as_str().expect("signed hmac");
        assert_eq!(hmac.len(), 64);
        assert!(hmac.chars().all(|c| c.is_ascii_hexdigit()));
        verify_value(&signed).unwrap();
        unsafe {
            std::env::remove_var("FLEET_HANDSHAKE_SECRET");
        }
    }

    #[test]
    fn verify_value_rejects_same_length_forged_hmac() {
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
        let good = signed["hmac"].as_str().expect("signed hmac").to_string();
        assert_eq!(good.len(), 64);
        let mut forged = signed;
        forged["hmac"] = Value::String("0".repeat(64));
        assert_eq!(forged["hmac"].as_str().unwrap().len(), good.len());
        assert_ne!(forged["hmac"].as_str().unwrap(), good.as_str());
        let err = verify_value(&forged).expect_err("same-length forged hmac");
        assert!(
            err.to_string().contains("invalid handshake hmac"),
            "{err}"
        );
        unsafe {
            std::env::remove_var("FLEET_HANDSHAKE_SECRET");
        }
    }

    #[test]
    fn verify_value_rejects_tampered_payload_with_intact_hmac() {
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
        let hmac = signed["hmac"].clone();
        let mut tampered = signed;
        tampered["job_id"] = json!("j2");
        tampered["hmac"] = hmac;
        let err = verify_value(&tampered).expect_err("tampered job_id");
        assert!(
            err.to_string().contains("invalid handshake hmac"),
            "{err}"
        );
        unsafe {
            std::env::remove_var("FLEET_HANDSHAKE_SECRET");
        }
    }

    #[test]
    fn sign_and_verify_fail_closed_without_secret() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let prev = stored_secret();
        if let Ok(mut g) = SECRET.lock() {
            *g = None;
        }
        unsafe {
            std::env::remove_var("FLEET_HANDSHAKE_SECRET");
        }
        let body = json!({
            "action": "provision",
            "job_id": "j1",
            "req_id": "r1"
        });
        assert!(sign_value(body.clone()).is_err());
        assert!(verify_value(&body).is_err());
        if let Ok(mut g) = SECRET.lock() {
            *g = prev;
        }
    }

    #[test]
    fn init_from_env_strips_environ_keeps_secret() {
        let guard = HandshakeEnvGuard::acquire();
        guard.clear_stored();
        guard.set_env(Some("init-secret"));
        init_from_env().expect("init_from_env");
        assert!(
            std::env::var("FLEET_HANDSHAKE_SECRET").is_err(),
            "secret must not remain in the process environ"
        );
        assert_eq!(handshake_secret().as_deref(), Some("init-secret"));
    }

    // set_var does not write the execve block that /proc/*/environ shows.
    #[cfg(target_os = "linux")]
    #[test]
    fn init_from_env_child_execve_wipes_proc_environ() {
        const SECRET: &str = "child-execve-wipe-secret";
        const SENTINEL: &str = "handshake-child-wipe-ok";
        const CHILD_FLAG: &str = "FLEET_HANDSHAKE_CHILD_WIPE";

        if std::env::var_os(CHILD_FLAG).is_some() {
            let before = std::fs::read("/proc/self/environ").expect("read /proc/self/environ");
            assert!(
                before.windows(SECRET.len()).any(|w| w == SECRET.as_bytes()),
                "child execve env must contain the secret before init_from_env"
            );
            init_from_env().expect("init_from_env");
            // init_from_env sets PR_SET_DUMPABLE=0, which makes /proc/self/environ
            // EACCES even to this process. Re-enable dumpable only to observe the wipe.
            let rc = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 1, 0, 0, 0) };
            assert_eq!(rc, 0, "temporarily re-enable dumpable to read environ");
            let after = std::fs::read("/proc/self/environ").expect("read /proc/self/environ");
            assert!(
                !after.windows(SECRET.len()).any(|w| w == SECRET.as_bytes()),
                "secret must not remain in /proc/self/environ"
            );
            println!("{SENTINEL}");
            return;
        }

        let exe = std::env::current_exe().expect("current_exe");
        let current = std::thread::current();
        let test_name = current.name().expect("rustc test thread name");
        let output = std::process::Command::new(exe)
            .env(CHILD_FLAG, "1")
            .env("FLEET_HANDSHAKE_SECRET", SECRET)
            .args(["--exact", test_name, "--nocapture"])
            .output()
            .expect("spawn handshake child");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "child failed: status={:?} stdout={stdout} stderr={stderr}",
            output.status
        );
        assert!(
            stdout.contains(SENTINEL),
            "child did not run wipe check: stdout={stdout} stderr={stderr}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn init_from_env_sets_dumpable_0() {
        let _guard = HandshakeEnvGuard::acquire();
        init_from_env().expect("init_from_env");
        let dumpable = unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) };
        assert_eq!(dumpable, 0, "PR_SET_DUMPABLE=0 after init_from_env");
    }

    #[test]
    fn init_from_env_blank_or_missing_secret_does_not_store() {
        let guard = HandshakeEnvGuard::acquire();
        let body = json!({
            "action": "provision",
            "job_id": "j1",
            "req_id": "r1"
        });
        for value in [None, Some(" \t ")] {
            guard.clear_stored();
            guard.set_env(value);
            init_from_env().expect("init_from_env");
            assert!(
                handshake_secret().is_none(),
                "blank/missing env must not leave a handshake secret"
            );
            assert!(
                sign_value(body.clone()).is_err(),
                "sign_value must fail without a stored secret"
            );
        }
    }
}
