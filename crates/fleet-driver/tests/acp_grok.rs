//! Live Grok Build ACP e2e.
//!
//! Requires `grok` on PATH or `~/.grok/bin/grok` and `grok login` / auth.json.
//! Run with `cargo test -p fleet-driver --test acp_grok -- --ignored --nocapture`.

use std::collections::HashMap;
use std::time::Duration;

use fleet_driver::acp::{self, AcpRun};
use fleet_driver::harness;
use tempfile::tempdir;

#[ignore]
#[tokio::test]
async fn grok_tiny_prompt_over_acp() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("README.md"), "fleet grok e2e\n").unwrap();

    let spec =
        harness::resolve("grok", None, None, true, None, None, Some("low")).expect("resolve grok");

    let out = acp::run_session(AcpRun {
        harness: spec,
        cwd: dir.path().to_path_buf(),
        session_dir: dir.path().to_path_buf(),
        prompt: "Reply with exactly the single word pong and nothing else. Do not use tools."
            .into(),
        model: None,
        auto_approve: true,
        timeout: Duration::from_secs(120),
        extra_env: HashMap::new(),
    })
    .await
    .expect("run_session");

    assert!(
        out.ok,
        "grok ACP failed: {:?} stderr={}",
        out.error, out.stderr_tail
    );
    assert_eq!(out.stop_reason.as_deref(), Some("end_turn"));
    let t = out.transcript.to_ascii_lowercase();
    assert!(
        t.contains("pong"),
        "expected pong in transcript, got {:?}",
        out.transcript
    );
}
