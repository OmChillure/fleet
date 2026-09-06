//! Live Grok Build ACP e2e.
//!
//! Live tests require `grok` on PATH or `~/.grok/bin/grok`, and
//! `grok login` / auth.json. Run with
//! `cargo test -p fleet-driver --test acp_grok -- --ignored --nocapture`.

use std::collections::HashMap;
use std::time::Duration;

use fleet_driver::acp::{self, AcpRun};
use fleet_driver::harness;
use tempfile::tempdir;

#[ignore = "live Grok ACP"]
#[tokio::test]
async fn grok_tiny_prompt_over_acp() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("README.md"), "fleet grok e2e\n").unwrap();

    let spec =
        harness::resolve("grok", None, None, true, None, None, Some("low")).expect("resolve grok");
    assert_eq!(spec.name, "grok");
    assert!(
        spec.command == "grok"
            || spec.command.ends_with("/grok")
            || spec.command.ends_with("\\grok")
            || spec.command.ends_with("grok.exe"),
        "expected grok argv command, got {}",
        spec.command
    );
    assert!(
        spec.args.iter().any(|a| a == "agent")
            && spec.args.iter().any(|a| a == "--no-leader")
            && spec.args.iter().any(|a| a == "--always-approve")
            && spec.args.windows(2).any(|w| w[0] == "--reasoning-effort" && w[1] == "low")
            && spec.args.iter().any(|a| a == "stdio"),
        "expected grok agent argv, got {:?}",
        spec.args
    );

    let out = match acp::run_session(AcpRun {
        harness: spec.clone(),
        cwd: dir.path().to_path_buf(),
        session_dir: dir.path().to_path_buf(),
        prompt: "Reply briefly. Do not use tools.".into(),
        model: None,
        auto_approve: true,
        timeout: Duration::from_secs(120),
        extra_env: HashMap::new(),
        live_out: None,
    })
    .await
    {
        Ok(out) => out,
        Err(e) => panic!(
            "spawn grok failed: {e:#}\n\
             install grok on PATH or ~/.grok/bin/grok, run `grok login`, \
             set FLEET_GROK_BIN if needed; then \
             cargo test -p fleet-driver --test acp_grok -- --ignored --nocapture"
        ),
    };

    assert!(
        out.ok,
        "grok ACP failed: {:?} stderr={}",
        out.error, out.stderr_tail
    );
    assert_eq!(out.stop_reason.as_deref(), Some("end_turn"));
    assert!(
        !out.transcript.trim().is_empty(),
        "expected non-empty transcript, got {:?} stderr={}",
        out.transcript,
        out.stderr_tail
    );
    let mut expected = vec![spec.command.clone()];
    expected.extend(spec.args.iter().cloned());
    assert_eq!(out.command, expected);
}

fn write_local_agent(dir: &tempfile::TempDir, source: &str) -> harness::HarnessSpec {
    let path = dir.path().join("local_acp_agent.py");
    std::fs::write(&path, source).unwrap();
    harness::HarnessSpec {
        name: "mock".into(),
        command: "python3".into(),
        args: vec![path.display().to_string()],
    }
}

fn local_run(dir: &tempfile::TempDir, spec: harness::HarnessSpec) -> AcpRun {
    AcpRun {
        harness: spec,
        cwd: dir.path().to_path_buf(),
        session_dir: dir.path().to_path_buf(),
        prompt: "hello".into(),
        model: None,
        auto_approve: true,
        timeout: Duration::from_secs(10),
        extra_env: HashMap::new(),
        live_out: None,
    }
}

const LSP_FRAMING_AGENT: &str = r#"
import json, sys

def read_msg():
    while True:
        line = sys.stdin.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            continue
        try:
            return json.loads(line)
        except json.JSONDecodeError:
            continue

def write_lsp(obj):
    body = json.dumps(obj, separators=(",", ":")).encode("utf-8")
    sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n".encode("ascii") + body)
    sys.stdout.buffer.flush()

while True:
    msg = read_msg()
    if msg is None:
        break
    method = msg.get("method") or ""
    req_id = msg.get("id")
    if req_id is None:
        continue
    if method == "initialize":
        write_lsp({
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "protocolVersion": 1,
                "agentCapabilities": {"loadSession": False},
                "agentInfo": {"name": "lsp-local", "version": "0.0.1"},
                "authMethods": [{"id": "cached_token", "name": "Cached"}],
            },
        })
    elif method == "authenticate":
        write_lsp({"jsonrpc": "2.0", "id": req_id, "result": {}})
    elif method == "session/new":
        write_lsp({"jsonrpc": "2.0", "id": req_id, "result": {"sessionId": "sess-lsp-1"}})
    elif method == "session/prompt":
        write_lsp({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "sess-lsp-1",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": "hello from lsp"},
                },
            },
        })
        write_lsp({"jsonrpc": "2.0", "id": req_id, "result": {"stopReason": "end_turn"}})
    else:
        write_lsp({"jsonrpc": "2.0", "id": req_id, "result": {}})
"#;

const AUTH_ERROR_AGENT: &str = r#"
import json, sys

def read_msg():
    while True:
        line = sys.stdin.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            continue
        try:
            return json.loads(line)
        except json.JSONDecodeError:
            continue

def write_msg(obj):
    sys.stdout.write(json.dumps(obj, separators=(",", ":")))
    sys.stdout.write("\n")
    sys.stdout.flush()

while True:
    msg = read_msg()
    if msg is None:
        break
    method = msg.get("method") or ""
    req_id = msg.get("id")
    if req_id is None:
        continue
    if method == "initialize":
        write_msg({
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "protocolVersion": 1,
                "agentCapabilities": {"loadSession": False},
                "authMethods": [{"id": "cached_token", "name": "Cached"}],
            },
        })
    elif method == "authenticate":
        write_msg({
            "jsonrpc": "2.0",
            "id": req_id,
            "error": {"code": -32000, "message": "authenticate rejected"},
        })
    else:
        write_msg({"jsonrpc": "2.0", "id": req_id, "result": {}})
"#;

#[tokio::test]
async fn acp_lsp_content_length_framing_roundtrip() {
    let dir = tempdir().unwrap();
    let out = acp::run_session(local_run(&dir, write_local_agent(&dir, LSP_FRAMING_AGENT)))
        .await
        .expect("run_session");
    assert!(out.ok, "error={:?} stderr={}", out.error, out.stderr_tail);
    assert!(!out.transcript.trim().is_empty(), "transcript={:?}", out.transcript);
}

#[tokio::test]
async fn run_session_authenticate_rpc_error_is_not_ok() {
    let dir = tempdir().unwrap();
    let result = acp::run_session(local_run(&dir, write_local_agent(&dir, AUTH_ERROR_AGENT))).await;
    match result {
        Ok(out) => {
            assert!(!out.ok, "authenticate rpc error must not be ok: {:?}", out.error);
            assert!(
                out.error
                    .as_deref()
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .contains("authenticate"),
                "error={:?}",
                out.error
            );
        }
        Err(_) => {}
    }
}

#[tokio::test]
async fn live_grok_missing_bin_fails_closed() {
    let dir = tempdir().unwrap();
    let spec = harness::HarnessSpec {
        name: "grok".into(),
        command: "/no/such/grok".into(),
        args: vec![
            "agent".into(),
            "--no-leader".into(),
            "--always-approve".into(),
            "stdio".into(),
        ],
    };
    let err = acp::run_session(AcpRun {
        harness: spec,
        cwd: dir.path().to_path_buf(),
        session_dir: dir.path().to_path_buf(),
        prompt: "hello".into(),
        model: None,
        auto_approve: true,
        timeout: Duration::from_secs(5),
        extra_env: HashMap::new(),
        live_out: None,
    })
    .await
    .expect_err(
        "missing /no/such/grok must fail spawn; install grok on PATH or ~/.grok/bin/grok, run `grok login`",
    );
    let msg = format!("{err:#}");
    assert!(
        msg.contains("spawn") && msg.contains("/no/such/grok"),
        "expected named spawn fail, got {msg}"
    );
}
