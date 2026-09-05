//! Local Ollama tool loop on the runner. Keys never leave this box.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use tokio::process::Command;

use crate::orch::OrchClient;

const MAX_ROUNDS: usize = 10;

pub async fn run_ollama(
    work_dir: &Path,
    model: &str,
    prompt: &str,
    job_timeout: Duration,
    client: &OrchClient,
) -> Result<String> {
    run_ollama_on(work_dir, model, prompt, job_timeout, client, None).await
}

/// Same as [`run_ollama`] but talks to `base_url` instead of env / :11434.
/// Tests pass a [`crate::mock_ollama::MockOllama`] URL here.
pub async fn run_ollama_on(
    work_dir: &Path,
    model: &str,
    prompt: &str,
    job_timeout: Duration,
    client: &OrchClient,
    base_url: Option<&str>,
) -> Result<String> {
    if model.trim().is_empty() {
        return Err(anyhow!("ollama harness needs FLEET_MODEL"));
    }
    let base = base_url
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_end_matches('/').to_string())
        .or_else(|| {
            std::env::var("OLLAMA_BASE_URL")
                .ok()
                .map(|s| s.trim().trim_end_matches('/').to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "http://127.0.0.1:11434".into());
    let system = std::fs::read_to_string(work_dir.join("PERSONA.md")).unwrap_or_default();
    let team = std::fs::read_to_string(work_dir.join("TEAM.md")).unwrap_or_default();
    let mut messages = vec![
        json!({
            "role": "system",
            "content": format!(
                "{}\n\nYou are one specialist on a fleet run. Work only in this directory.\n\
                 Shared artifacts are in work/. Write result.md when done.\n\
                 To call a connected app: tool `fleet_io` op=integration.\n\
                 To talk to teammates: fleet_io op=team_chat_post / team_chat_list.\n\
                 Reviewers bounce work with fleet_io op=request_changes.\n\n{team}",
                if system.trim().is_empty() { "You are a competent specialist.".into() } else { system }
            )
        }),
        json!({"role": "user", "content": prompt}),
    ];
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;
    let deadline = tokio::time::Instant::now() + job_timeout;
    let mut last = String::new();
    for _ in 1..=MAX_ROUNDS {
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        let res = http
            .post(format!("{base}/v1/chat/completions"))
            .json(&json!({
                "model": model,
                "messages": messages,
                "tools": tools_schema(),
                "temperature": 0.2,
            }))
            .send()
            .await?;
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("ollama {status}: {}", body.chars().take(400).collect::<String>()));
        }
        let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let msg = v
            .pointer("/choices/0/message")
            .cloned()
            .unwrap_or(Value::Null);
        if let Some(text) = msg.get("content").and_then(|c| c.as_str()) {
            last = text.to_string();
        }
        let calls = msg
            .get("tool_calls")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        messages.push(msg);
        if calls.is_empty() {
            break;
        }
        for call in calls {
            let id = call.get("id").and_then(|v| v.as_str()).unwrap_or("c");
            let name = call
                .pointer("/function/name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let args_raw = call
                .pointer("/function/arguments")
                .and_then(|v| v.as_str())
                .unwrap_or("{}");
            let args: Value = serde_json::from_str(args_raw).unwrap_or(json!({}));
            let out = exec_tool(work_dir, name, &args, client).await;
            messages.push(json!({
                "role": "tool",
                "tool_call_id": id,
                "content": out,
            }));
        }
    }
    if !work_dir.join("result.md").is_file() && !last.trim().is_empty() {
        let _ = std::fs::write(work_dir.join("result.md"), &last);
    }
    Ok(if last.trim().is_empty() {
        "ollama finished".into()
    } else {
        last.chars().take(400).collect()
    })
}

fn tools_schema() -> Value {
    json!([
        {"type":"function","function":{"name":"list_dir","parameters":{"type":"object","properties":{"path":{"type":"string"}}}}},
        {"type":"function","function":{"name":"read_file","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}},
        {"type":"function","function":{"name":"write_file","parameters":{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}}},
        {"type":"function","function":{"name":"run_bash","parameters":{"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}}},
        {"type":"function","function":{"name":"fleet_io","parameters":{"type":"object","properties":{"op":{"type":"string"},"provider":{"type":"string"},"operation":{"type":"string"},"args":{"type":"object"},"to":{"type":"string"},"body":{"type":"string"},"reason":{"type":"string"}},"required":["op"]}}}
    ])
}

async fn exec_tool(root: &Path, name: &str, args: &Value, client: &OrchClient) -> String {
    match name {
        "list_dir" => {
            let path = resolve(root, args.get("path").and_then(|v| v.as_str()).unwrap_or("."));
            match std::fs::read_dir(&path) {
                Ok(rd) => {
                    let mut names: Vec<String> = rd
                        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
                        .collect();
                    names.sort();
                    names.join("\n")
                }
                Err(e) => format!("error: {e}"),
            }
        }
        "read_file" => {
            let Some(p) = args.get("path").and_then(|v| v.as_str()) else {
                return "error: path required".into();
            };
            match std::fs::read_to_string(resolve(root, p)) {
                Ok(s) => s.chars().take(16_000).collect(),
                Err(e) => format!("error: {e}"),
            }
        }
        "write_file" => {
            let Some(p) = args.get("path").and_then(|v| v.as_str()) else {
                return "error: path required".into();
            };
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let dest = resolve(root, p);
            if let Some(parent) = dest.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::write(&dest, content) {
                Ok(()) => format!("wrote {}", dest.display()),
                Err(e) => format!("error: {e}"),
            }
        }
        "run_bash" => {
            let Some(cmd) = args.get("command").and_then(|v| v.as_str()) else {
                return "error: command required".into();
            };
            match Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(root)
                .output()
                .await
            {
                Ok(o) => {
                    let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
                    if !o.stderr.is_empty() {
                        s.push_str("\n[stderr]\n");
                        s.push_str(&String::from_utf8_lossy(&o.stderr));
                    }
                    s.chars().take(16_000).collect()
                }
                Err(e) => format!("error: {e}"),
            }
        }
        "fleet_io" => match client.post_io(args.clone()).await {
            Ok(v) => v.to_string(),
            Err(e) => format!("error: {e}"),
        },
        other => format!("error: unknown tool {other}"),
    }
}

fn resolve(root: &Path, rel: &str) -> PathBuf {
    let p = Path::new(rel);
    if p.is_absolute() {
        root.join(p.file_name().unwrap_or_default())
    } else {
        root.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_stays_under_root() {
        let root = Path::new("/tmp/ws");
        assert_eq!(resolve(root, "work/a.rs"), PathBuf::from("/tmp/ws/work/a.rs"));
        assert_eq!(
            resolve(root, "/etc/passwd").file_name().unwrap(),
            "passwd"
        );
    }

    #[tokio::test]
    async fn mock_ollama_writes_work_and_result() {
        let ollama = crate::mock_ollama::MockOllama::spawn().expect("bind");
        let dir = tempfile::tempdir().unwrap();
        let client = crate::orch::OrchClient::new("http://127.0.0.1:1", "p", "s", "tok").unwrap();
        let out = run_ollama_on(
            dir.path(),
            crate::mock_ollama::TEST_MODEL,
            "do the work",
            Duration::from_secs(5),
            &client,
            Some(&ollama.base_url()),
        )
        .await
        .expect("loop");
        assert!(out.contains("Task completed"), "loop summary: {out}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("work/out.txt")).unwrap(),
            "from-tiny"
        );
        assert!(dir.path().join("result.md").is_file());
        assert!(ollama.rounds() >= 2);
    }
}
