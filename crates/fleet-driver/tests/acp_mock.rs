//! ACP client ↔ mock-acp-agent (no LLM).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use fleet_driver::acp::{self, AcpRun};
use fleet_driver::harness::HarnessSpec;
use tempfile::tempdir;

fn mock_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mock-acp-agent"))
}

fn spec_with_args(args: Vec<String>) -> HarnessSpec {
    HarnessSpec {
        name: "mock".into(),
        command: mock_bin().display().to_string(),
        args,
    }
}

fn base_run(dir: &std::path::Path, spec: HarnessSpec) -> AcpRun {
    AcpRun {
        harness: spec,
        cwd: dir.to_path_buf(),
        prompt: "Write a short greeting.".into(),
        model: Some("mock-fast".into()),
        auto_approve: true,
        timeout: Duration::from_secs(10),
        extra_env: HashMap::new(),
    }
}

#[tokio::test]
async fn mock_happy_path_transcript() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("spec.md"), "say hello").unwrap();
    let out = acp::run_session(base_run(dir.path(), spec_with_args(vec![])))
        .await
        .unwrap();
    assert!(out.ok, "error={:?} stderr={}", out.error, out.stderr_tail);
    assert_eq!(out.session_id.as_deref(), Some("sess-mock-1"));
    assert_eq!(out.stop_reason.as_deref(), Some("end_turn"));
    assert!(
        out.transcript.contains("hello from mock agent"),
        "transcript={}",
        out.transcript
    );
    assert!(out.transcript.contains("list_dir"));
}

#[tokio::test]
async fn mock_fail_is_not_ok() {
    let dir = tempdir().unwrap();
    let out = acp::run_session(base_run(dir.path(), spec_with_args(vec!["--fail".into()])))
        .await
        .unwrap();
    assert!(!out.ok);
    assert!(
        out.error
            .as_deref()
            .unwrap_or("")
            .contains("forced failure")
            || out.error.as_deref().unwrap_or("").contains("rpc error"),
        "error={:?}",
        out.error
    );
}

#[tokio::test]
async fn mock_timeout() {
    let dir = tempdir().unwrap();
    let mut run = base_run(dir.path(), spec_with_args(vec!["--hang".into()]));
    run.timeout = Duration::from_millis(400);
    let out = acp::run_session(run).await.unwrap();
    assert!(!out.ok);
    assert!(
        out.error
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase()
            .contains("timed out")
            || out.stop_reason.as_deref() == Some("timeout"),
        "error={:?} stop={:?}",
        out.error,
        out.stop_reason
    );
}

#[tokio::test]
async fn mock_permission_auto_approve() {
    let dir = tempdir().unwrap();
    let out = acp::run_session(base_run(
        dir.path(),
        spec_with_args(vec!["--permission".into()]),
    ))
    .await
    .unwrap();
    assert!(out.ok, "error={:?} stderr={}", out.error, out.stderr_tail);
    assert!(out.transcript.contains("hello from mock agent"));
}
