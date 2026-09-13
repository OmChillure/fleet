//! Minimal ACP agent for tests (JSON-RPC, one object per line on stdio).
//!
//! Flags / env:
//!   --fail / MOCK_ACP_FAIL=1         RPC error on session/prompt
//!   --permission / MOCK_ACP_PERMISSION=1
//!       request permission during prompt; continue only on selected allow-*
//!   --hang / MOCK_ACP_HANG=1         never complete session/prompt
//!   --terminal / MOCK_ACP_TERMINAL=1  exercise client terminal/* methods
//!   --slow-ms N / MOCK_ACP_SLOW_MS   delay before prompt result

use std::env;
use std::io::{self, BufRead, Write};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

fn flag(name: &str, env_key: &str) -> bool {
    env::args().any(|a| a == name)
        || matches!(
            env::var(env_key).ok().as_deref(),
            Some("1") | Some("true") | Some("yes") | Some("on")
        )
}

fn slow_ms() -> u64 {
    env::args()
        .position(|a| a == "--slow-ms")
        .and_then(|i| env::args().nth(i + 1))
        .or_else(|| env::var("MOCK_ACP_SLOW_MS").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

fn read_msg(stdin: &mut impl BufRead) -> io::Result<Option<Value>> {
    loop {
        let mut line = String::new();
        let n = stdin.read_line(&mut line)?;
        if n == 0 {
            return Ok(None);
        }
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(t) {
            Ok(v) => return Ok(Some(v)),
            Err(_) => continue,
        }
    }
}

fn write_msg(stdout: &mut impl Write, obj: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *stdout, obj)?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}

fn main() -> io::Result<()> {
    let fail = flag("--fail", "MOCK_ACP_FAIL");
    let permission = flag("--permission", "MOCK_ACP_PERMISSION");
    let hang = flag("--hang", "MOCK_ACP_HANG");
    let terminal = flag("--terminal", "MOCK_ACP_TERMINAL");
    let delay = Duration::from_millis(slow_ms());

    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let mut stdout = io::stdout().lock();

    while let Some(msg) = read_msg(&mut stdin)? {
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let req_id = msg.get("id").cloned();

        // Notifications — ignore
        let Some(req_id) = req_id else {
            continue;
        };

        if method == "initialize" {
            write_msg(
                &mut stdout,
                &json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "protocolVersion": 1,
                        "agentCapabilities": { "loadSession": false },
                        "agentInfo": { "name": "mock-acp", "version": "0.0.1" },
                        "authMethods": [{ "id": "cached_token", "name": "Cached" }]
                    }
                }),
            )?;
        } else if method == "authenticate" {
            write_msg(
                &mut stdout,
                &json!({ "jsonrpc": "2.0", "id": req_id, "result": {} }),
            )?;
        } else if method == "session/new" {
            let cwd = msg
                .pointer("/params/cwd")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            write_msg(
                &mut stdout,
                &json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "sessionId": "sess-mock-1",
                        "configOptions": [{
                            "id": "model",
                            "name": "Model",
                            "category": "model",
                            "type": "select",
                            "currentValue": "mock-fast",
                            "options": [
                                { "value": "mock-fast", "name": "Fast" },
                                { "value": "mock-strong", "name": "Strong" }
                            ]
                        }],
                        "meta": { "cwd": cwd }
                    }
                }),
            )?;
        } else if method == "session/set_config_option" {
            write_msg(
                &mut stdout,
                &json!({ "jsonrpc": "2.0", "id": req_id, "result": {} }),
            )?;
        } else if method == "session/prompt" {
            if hang {
                loop {
                    thread::sleep(Duration::from_secs(60));
                }
            }
            if !delay.is_zero() {
                thread::sleep(delay);
            }
            if fail {
                write_msg(
                    &mut stdout,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "error": { "code": -32000, "message": "mock agent forced failure" }
                    }),
                )?;
                continue;
            }
            if permission {
                write_msg(
                    &mut stdout,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": 9001,
                        "method": "session/request_permission",
                        "params": {
                            "sessionId": "sess-mock-1",
                            "toolCall": { "toolCallId": "tc-1", "title": "write hello.txt" },
                            "options": [
                                { "optionId": "allow-once", "name": "Allow once" },
                                { "optionId": "reject", "name": "Reject" }
                            ]
                        }
                    }),
                )?;
                // Wait for the client reply to id 9001. Only selected allow-* continues.
                let mut allowed = false;
                let mut selected_option = String::new();
                loop {
                    match read_msg(&mut stdin)? {
                        None => return Ok(()),
                        Some(resp) => {
                            if resp.get("id") == Some(&json!(9001)) {
                                let outcome = resp
                                    .pointer("/result/outcome/outcome")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let option_id = resp
                                    .pointer("/result/outcome/optionId")
                                    .or_else(|| resp.pointer("/result/outcome/option_id"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                if outcome == "selected"
                                    && option_id.to_ascii_lowercase().contains("allow")
                                {
                                    allowed = true;
                                    selected_option = option_id.to_string();
                                }
                                break;
                            }
                            // Reply to unexpected client requests so the session does not stall.
                            if let Some(id) = resp.get("id")
                                && resp.get("method").is_some()
                            {
                                write_msg(
                                    &mut stdout,
                                    &json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
                                )?;
                            }
                        }
                    }
                }
                if !allowed {
                    write_msg(
                        &mut stdout,
                        &json!({
                            "jsonrpc": "2.0",
                            "id": req_id,
                            "result": { "stopReason": "cancelled" }
                        }),
                    )?;
                    continue;
                }
                write_msg(
                    &mut stdout,
                    &json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": "sess-mock-1",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {
                                    "type": "text",
                                    "text": format!("permission selected: {selected_option}")
                                }
                            }
                        }
                    }),
                )?;
            }
            if terminal {
                write_msg(
                    &mut stdout,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": 9100,
                        "method": "terminal/create",
                        "params": {
                            "sessionId": "sess-mock-1",
                            "command": "echo",
                            "args": ["hello-from-terminal"]
                        }
                    }),
                )?;
                let term_id = loop {
                    match read_msg(&mut stdin)? {
                        None => return Ok(()),
                        Some(resp) => {
                            if resp.get("id") == Some(&json!(9100)) {
                                break resp
                                    .pointer("/result/terminalId")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                            }
                            if let Some(id) = resp.get("id")
                                && resp.get("method").is_some()
                            {
                                write_msg(
                                    &mut stdout,
                                    &json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
                                )?;
                            }
                        }
                    }
                };
                write_msg(
                    &mut stdout,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": 9101,
                        "method": "terminal/wait_for_exit",
                        "params": { "terminalId": term_id }
                    }),
                )?;
                loop {
                    match read_msg(&mut stdin)? {
                        None => return Ok(()),
                        Some(resp) if resp.get("id") == Some(&json!(9101)) => break,
                        Some(resp) => {
                            if let Some(id) = resp.get("id")
                                && resp.get("method").is_some()
                            {
                                write_msg(
                                    &mut stdout,
                                    &json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
                                )?;
                            }
                        }
                    }
                }
                write_msg(
                    &mut stdout,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": 9102,
                        "method": "terminal/output",
                        "params": { "terminalId": term_id }
                    }),
                )?;
                let out_text = loop {
                    match read_msg(&mut stdin)? {
                        None => return Ok(()),
                        Some(resp) if resp.get("id") == Some(&json!(9102)) => {
                            break resp
                                .pointer("/result/output")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                        }
                        Some(resp) => {
                            if let Some(id) = resp.get("id")
                                && resp.get("method").is_some()
                            {
                                write_msg(
                                    &mut stdout,
                                    &json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
                                )?;
                            }
                        }
                    }
                };
                write_msg(
                    &mut stdout,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": 9103,
                        "method": "terminal/release",
                        "params": { "terminalId": term_id }
                    }),
                )?;
                loop {
                    match read_msg(&mut stdin)? {
                        None => return Ok(()),
                        Some(resp) if resp.get("id") == Some(&json!(9103)) => break,
                        Some(resp) => {
                            if let Some(id) = resp.get("id")
                                && resp.get("method").is_some()
                            {
                                write_msg(
                                    &mut stdout,
                                    &json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
                                )?;
                            }
                        }
                    }
                }
                write_msg(
                    &mut stdout,
                    &json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": "sess-mock-1",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {
                                    "type": "text",
                                    "text": format!("terminal said: {}", out_text.trim())
                                }
                            }
                        }
                    }),
                )?;
            }
            write_msg(
                &mut stdout,
                &json!({
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": "sess-mock-1",
                        "update": {
                            "sessionUpdate": "agent_message_chunk",
                            "content": { "type": "text", "text": "hello from mock agent" }
                        }
                    }
                }),
            )?;
            write_msg(
                &mut stdout,
                &json!({
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": "sess-mock-1",
                        "update": {
                            "sessionUpdate": "tool_call",
                            "title": "list_dir"
                        }
                    }
                }),
            )?;
            write_msg(
                &mut stdout,
                &json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": { "stopReason": "end_turn" }
                }),
            )?;
        } else {
            write_msg(
                &mut stdout,
                &json!({ "jsonrpc": "2.0", "id": req_id, "result": {} }),
            )?;
        }
    }
    Ok(())
}
