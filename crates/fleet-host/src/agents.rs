//! Official agent CLIs on this box. Heartbeat reports install + login only.
//! Never put access tokens, refresh tokens, or API keys on the wire.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

fn run_stdout_timeout(bin: &Path, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut buf = String::new();
                child.stdout.as_mut()?.read_to_string(&mut buf).ok()?;
                return Some(buf);
            }
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(40)),
            Err(_) => {
                let _ = child.kill();
                return None;
            }
        }
    }
}

/// Snapshot for one official CLI. `models` is a cache; tokens never appear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProbe {
    pub id: &'static str,
    pub name: &'static str,
    pub installed: bool,
    pub logged_in: bool,
    pub models: Vec<String>,
    pub model_details: Vec<ModelDetail>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModelDetail {
    pub name: String,
    pub efforts: Vec<EffortOpt>,
    pub default_effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EffortOpt {
    pub value: String,
    pub label: Option<String>,
}

static PROBE_CACHE: std::sync::Mutex<Option<(Instant, Vec<AgentProbe>)>> =
    std::sync::Mutex::new(None);

pub fn probe_all() -> Vec<AgentProbe> {
    if let Ok(g) = PROBE_CACHE.lock() {
        if let Some((at, cached)) = g.as_ref() {
            if at.elapsed() < Duration::from_secs(45) {
                return cached.clone();
            }
        }
    }
    let fresh = vec![probe_grok(), probe_claude(), probe_openai(), probe_ollama()];
    if let Ok(mut g) = PROBE_CACHE.lock() {
        *g = Some((Instant::now(), fresh.clone()));
    }
    fresh
}

pub fn invalidate_probe_cache() {
    if let Ok(mut g) = PROBE_CACHE.lock() {
        *g = None;
    }
}

/// Pull a model name or URL with the official `ollama` CLI.
pub fn ollama_pull(spec: &str) -> Result<(), String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("empty model spec".into());
    }
    if spec.len() > 512
        || spec
            .chars()
            .any(|c| c.is_control() || c == ';' || c == '|' || c == '&')
    {
        return Err("model spec looks unsafe".into());
    }
    let bin = which("ollama").ok_or_else(|| "ollama is not installed".to_string())?;
    let text = run_stdout_timeout(&bin, &["pull", spec], Duration::from_secs(600))
        .ok_or_else(|| format!("ollama pull {spec} failed or timed out"))?;
    let _ = text;
    Ok(())
}

pub fn heartbeat_agents() -> Value {
    Value::Array(
        probe_all()
            .into_iter()
            .map(|a| {
                json!({
                    "id": a.id,
                    "name": a.name,
                    "installed": a.installed,
                    "logged_in": a.logged_in,
                    "models": a.models,
                    "model_details": a.model_details.iter().map(|d| json!({
                        "name": d.name,
                        "efforts": d.efforts.iter().map(|e| json!({
                            "value": e.value,
                            "label": e.label,
                        })).collect::<Vec<_>>(),
                        "default_effort": d.default_effort,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn which(name: &str) -> Option<PathBuf> {
    crate::software::resolve_bin(name, std::env::current_exe().ok().as_deref())
}

fn probe_grok() -> AgentProbe {
    let home = home_dir();
    let bin = which("grok").or_else(|| {
        home.as_ref()
            .map(|h| h.join(".grok/bin/grok"))
            .filter(|p| p.is_file())
    });
    let installed = bin.is_some();
    let logged_in = home
        .as_ref()
        .map(|h| grok_session_fresh(&h.join(".grok/auth.json")))
        .unwrap_or(false);
    let details = if logged_in {
        grok_model_details()
    } else {
        vec![]
    };
    let models: Vec<String> = if !details.is_empty() {
        details.iter().map(|d| d.name.clone()).collect()
    } else if logged_in {
        grok_cli_models().unwrap_or_default()
    } else {
        vec![]
    };
    AgentProbe {
        id: "grok",
        name: "Grok",
        installed,
        logged_in,
        models,
        model_details: details,
    }
}

/// Parse `grok models` (default first). Never include secrets.
pub fn parse_grok_models_output(stdout: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let t = line.trim();
        let t = t.trim_start_matches('*').trim_start_matches('-').trim();
        let name = t.split_whitespace().next().unwrap_or("");
        if name.is_empty() {
            continue;
        }
        if !(name.starts_with("grok-") || name.starts_with("sxs-") || name.starts_with("claude-")) {
            continue;
        }
        if !out.iter().any(|m| m == name) {
            out.push(name.to_string());
        }
    }
    out
}

/// Official CLI cache (`~/.grok/models_cache.json`) — per-model effort menus.
pub fn parse_grok_models_cache(raw: &str) -> Vec<ModelDetail> {
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return vec![];
    };
    let Some(map) = v.get("models").and_then(|m| m.as_object()) else {
        return vec![];
    };
    let mut out = Vec::new();
    for (name, entry) in map {
        let info = entry.get("info").unwrap_or(entry);
        let efforts_raw = info
            .get("reasoning_efforts")
            .and_then(|e| e.as_array())
            .cloned()
            .unwrap_or_default();
        let mut efforts = Vec::new();
        let mut default_effort = None;
        for item in efforts_raw {
            let value = item
                .get("value")
                .or_else(|| item.get("id"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if value.is_empty() {
                continue;
            }
            if item
                .get("default")
                .and_then(|d| d.as_bool())
                .unwrap_or(false)
                && default_effort.is_none()
            {
                default_effort = Some(value.clone());
            }
            let label = item
                .get("label")
                .and_then(|l| l.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            if !efforts.iter().any(|e: &EffortOpt| e.value == value) {
                efforts.push(EffortOpt { value, label });
            }
        }
        if default_effort.is_none() {
            default_effort = info
                .get("reasoning_effort")
                .and_then(|e| e.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
        }
        out.push(ModelDetail {
            name: name.trim().to_string(),
            efforts,
            default_effort,
        });
    }
    out.into_iter().filter(|d| !d.name.is_empty()).collect()
}

fn grok_model_details() -> Vec<ModelDetail> {
    let path = home_dir().map(|h| h.join(".grok/models_cache.json"));
    let from_cache = path
        .as_ref()
        .and_then(|p| fs::read_to_string(p).ok())
        .map(|s| parse_grok_models_cache(&s))
        .unwrap_or_default();
    let cli = grok_cli_models().unwrap_or_default();
    if cli.is_empty() {
        return from_cache;
    }
    let mut out = Vec::new();
    for name in &cli {
        if let Some(d) = from_cache.iter().find(|d| &d.name == name) {
            out.push(d.clone());
        } else {
            out.push(ModelDetail {
                name: name.clone(),
                efforts: vec![],
                default_effort: None,
            });
        }
    }
    out
}

fn grok_cli_models() -> Option<Vec<String>> {
    let bin = which("grok").or_else(|| {
        home_dir()
            .map(|h| h.join(".grok/bin/grok"))
            .filter(|p| p.is_file())
    })?;
    let text = run_stdout_timeout(&bin, &["models"], Duration::from_secs(3))?;
    let models = parse_grok_models_output(&text);
    if models.is_empty() {
        None
    } else {
        Some(models)
    }
}

fn env_model_list(var: &str) -> Option<Vec<String>> {
    let raw = std::env::var(var).ok()?;
    let models: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if models.is_empty() {
        None
    } else {
        Some(models)
    }
}

/// Official `--model` aliases. Never spawn `claude models` — that is not a
/// command; it starts an interactive session and hangs (especially on Windows).
const CLAUDE_MODEL_ALIASES: &[&str] = &["sonnet", "opus", "haiku", "fable"];
const CLAUDE_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

fn claude_cli_models() -> Option<Vec<String>> {
    env_model_list("CLAUDE_MODELS")
        .or_else(|| env_model_list("ANTHROPIC_MODELS"))
        .or_else(|| Some(CLAUDE_MODEL_ALIASES.iter().map(|s| (*s).to_string()).collect()))
}

fn claude_model_details(models: &[String]) -> Vec<ModelDetail> {
    let efforts: Vec<EffortOpt> = CLAUDE_EFFORTS
        .iter()
        .map(|v| EffortOpt {
            value: (*v).to_string(),
            label: None,
        })
        .collect();
    models
        .iter()
        .map(|name| ModelDetail {
            name: name.clone(),
            efforts: efforts.clone(),
            default_effort: Some("high".into()),
        })
        .collect()
}

/// Official `codex models` / `openai models` if present; else env.
fn openai_cli_models() -> Option<Vec<String>> {
    if let Some(from_env) = env_model_list("OPENAI_MODELS") {
        return Some(from_env);
    }
    for name in ["codex", "openai"] {
        let Some(bin) = which(name) else {
            continue;
        };
        let Some(text) = run_stdout_timeout(&bin, &["models"], Duration::from_secs(2)) else {
            continue;
        };
        let models = parse_cli_model_ids(&text, &["gpt-", "o1", "o3", "o4"]);
        if !models.is_empty() {
            return Some(models);
        }
    }
    None
}

/// Generic CLI list: one id per line, skip headings. Prefixes keep junk out.
pub fn parse_cli_model_ids(stdout: &str, prefixes: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let t = line.trim();
        let t = t.trim_start_matches('*').trim_start_matches('-').trim();
        let name = t.split_whitespace().next().unwrap_or("");
        if name.is_empty() || name.eq_ignore_ascii_case("name") {
            continue;
        }
        if !prefixes
            .iter()
            .any(|p| name.starts_with(p) || name.eq_ignore_ascii_case(p))
        {
            continue;
        }
        if !out.iter().any(|m| m == name) {
            out.push(name.to_string());
        }
    }
    out
}

/// True when auth.json has a session that is not past `expires_at`.
pub fn grok_session_fresh(path: &Path) -> bool {
    let Ok(raw) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<Value>(&raw) else {
        return false;
    };
    let Some(map) = v.as_object() else {
        return false;
    };
    for entry in map.values() {
        let Some(obj) = entry.as_object() else {
            continue;
        };
        let has_key = obj
            .get("key")
            .and_then(|k| k.as_str())
            .is_some_and(|s| !s.is_empty());
        if !has_key {
            continue;
        }
        match obj.get("expires_at").and_then(|e| e.as_str()) {
            None => return true,
            Some(exp) => {
                if DateTime::parse_from_rfc3339(exp)
                    .map(|t| t.with_timezone(&Utc) > Utc::now())
                    .unwrap_or(false)
                {
                    return true;
                }
            }
        }
    }
    false
}

fn probe_claude() -> AgentProbe {
    let installed = which("claude").is_some();
    let home = home_dir();
    let logged_in = home
        .as_ref()
        .map(|h| {
            claude_creds_present(&h.join(".claude.json"))
                || claude_creds_present(&h.join(".claude/.credentials.json"))
                || claude_creds_present(&h.join(".config/claude/.credentials.json"))
        })
        .unwrap_or(false);
    let models = if logged_in {
        claude_cli_models().unwrap_or_default()
    } else {
        vec![]
    };
    let model_details = if models.is_empty() {
        vec![]
    } else {
        claude_model_details(&models)
    };
    AgentProbe {
        id: "claude",
        name: "Claude",
        installed,
        logged_in,
        models,
        model_details,
    }
}

fn claude_creds_present(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    meta.is_file() && meta.len() > 2
}

fn probe_openai() -> AgentProbe {
    let installed = which("codex").is_some() || which("openai").is_some();
    let key = std::env::var("OPENAI_API_KEY")
        .ok()
        .is_some_and(|s| !s.trim().is_empty());
    let home = home_dir();
    let cli_session = home
        .as_ref()
        .map(|h| {
            claude_creds_present(&h.join(".codex/auth.json"))
                || claude_creds_present(&h.join(".openai/auth.json"))
        })
        .unwrap_or(false);
    let logged_in = key || cli_session;
    let models = if logged_in {
        openai_cli_models().unwrap_or_default()
    } else {
        vec![]
    };
    AgentProbe {
        id: "openai",
        name: "OpenAI",
        installed: installed || key,
        logged_in,
        models,
        model_details: vec![],
    }
}

fn probe_ollama() -> AgentProbe {
    let bin = which("ollama").is_some();
    let up = ollama_port_open();
    let installed = bin || up;
    let models = if up {
        ollama_list_models().unwrap_or_default()
    } else {
        vec![]
    };
    AgentProbe {
        id: "ollama",
        name: "Private (Ollama)",
        installed,
        logged_in: up,
        models,
        model_details: vec![],
    }
}

fn ollama_list_models() -> Option<Vec<String>> {
    let bin = which("ollama")?;
    let text = run_stdout_timeout(&bin, &["list"], Duration::from_secs(3))?;
    let mut models = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if i == 0 {
            continue;
        }
        let name = line.split_whitespace().next().unwrap_or("");
        if !name.is_empty() && name != "NAME" {
            models.push(name.to_string());
        }
    }
    if models.is_empty() {
        None
    } else {
        Some(models)
    }
}

fn ollama_port_open() -> bool {
    use std::net::{SocketAddr, TcpStream};
    use std::time::Duration;
    let addr = SocketAddr::from(([127, 0, 0, 1], 11434));
    TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}

fn sanitize_model_details(v: Option<&Value>) -> Value {
    let Some(arr) = v.and_then(|x| x.as_array()) else {
        return Value::Array(vec![]);
    };
    Value::Array(
        arr.iter()
            .filter_map(|item| {
                let obj = item.as_object()?;
                let name = obj.get("name")?.as_str()?.trim();
                if name.is_empty() {
                    return None;
                }
                let efforts = obj
                    .get("efforts")
                    .and_then(|e| e.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|e| {
                                let value = e
                                    .get("value")
                                    .or_else(|| e.get("id"))
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("")
                                    .trim();
                                if value.is_empty() {
                                    return None;
                                }
                                Some(json!({
                                    "value": value,
                                    "label": e.get("label").and_then(|l| l.as_str()),
                                }))
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                Some(json!({
                    "name": name,
                    "efforts": efforts,
                    "default_effort": obj.get("default_effort").and_then(|d| d.as_str()),
                }))
            })
            .collect(),
    )
}

/// Drop any secret-looking keys if a future probe adds them by mistake.
pub fn sanitize_agents_value(v: &Value) -> Value {
    let Some(arr) = v.as_array() else {
        return Value::Array(vec![]);
    };
    Value::Array(
        arr.iter()
            .filter_map(|item| {
                let obj = item.as_object()?;
                let id = obj.get("id")?.as_str()?.to_string();
                if id.trim().is_empty() {
                    return None;
                }
                let models = obj
                    .get("models")
                    .and_then(|m| m.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let model_details = sanitize_model_details(obj.get("model_details"));
                Some(json!({
                    "id": id,
                    "name": obj.get("name").and_then(|n| n.as_str()).unwrap_or(&id),
                    "installed": obj.get("installed").and_then(|b| b.as_bool()).unwrap_or(false),
                    "logged_in": obj.get("logged_in").and_then(|b| b.as_bool()).unwrap_or(false),
                    "models": models,
                    "model_details": model_details,
                }))
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn grok_missing_file_is_logged_out() {
        assert!(!grok_session_fresh(Path::new("/no/such/auth.json")));
    }

    #[test]
    fn grok_fresh_session() {
        let dir = std::env::temp_dir().join(format!("fleet-grok-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("auth.json");
        let future = (Utc::now() + chrono::Duration::hours(2)).to_rfc3339();
        let body = json!({
            "https://auth.x.ai::test": {
                "key": "not-a-real-key",
                "expires_at": future
            }
        });
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(body.to_string().as_bytes()).unwrap();
        assert!(grok_session_fresh(&path));
        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn grok_expired_session() {
        let dir = std::env::temp_dir().join(format!("fleet-grok-exp-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("auth.json");
        let past = (Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
        let body = json!({
            "https://auth.x.ai::test": {
                "key": "not-a-real-key",
                "expires_at": past
            }
        });
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(body.to_string().as_bytes()).unwrap();
        assert!(!grok_session_fresh(&path));
        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn parse_grok_models_lists_46_first() {
        let raw = "You are logged in with grok.com.\n\nDefault model: grok-4.6\n\nAvailable models:\n  * grok-4.6 (default)\n  - grok-4.5\n  - sxs-claude-opus-4-6\n";
        assert_eq!(
            parse_grok_models_output(raw),
            vec!["grok-4.6", "grok-4.5", "sxs-claude-opus-4-6"]
        );
    }

    #[test]
    fn parse_grok_cache_reads_xhigh() {
        let raw = r#"{
          "models": {
            "grok-4.6": {
              "info": {
                "reasoning_effort": "high",
                "reasoning_efforts": [
                  {"value":"xhigh","label":"Extra High Effort"},
                  {"value":"high","label":"High Effort","default":true},
                  {"value":"medium","label":"Medium Effort"},
                  {"value":"low","label":"Low Effort"}
                ]
              }
            }
          }
        }"#;
        let details = parse_grok_models_cache(raw);
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].name, "grok-4.6");
        assert_eq!(
            details[0]
                .efforts
                .iter()
                .map(|e| e.value.as_str())
                .collect::<Vec<_>>(),
            vec!["xhigh", "high", "medium", "low"]
        );
        assert_eq!(details[0].default_effort.as_deref(), Some("high"));
    }

    #[test]
    fn parse_cli_model_ids_skips_headings() {
        let raw =
            "NAME\n  - claude-sonnet-example\n  * claude-opus-example (default)\n  ignore-me\n";
        assert_eq!(
            parse_cli_model_ids(raw, &["claude-"]),
            vec!["claude-sonnet-example", "claude-opus-example"]
        );
    }

    #[test]
    fn sanitize_strips_secrets() {
        let dirty = json!([{
            "id": "grok",
            "name": "Grok",
            "installed": true,
            "logged_in": true,
            "key": "SECRET",
            "models": ["grok-4.5"]
        }]);
        let clean = sanitize_agents_value(&dirty);
        let obj = clean[0].as_object().unwrap();
        assert!(!obj.contains_key("key"));
        assert_eq!(obj["id"], "grok");
        assert_eq!(obj["models"][0], "grok-4.5");
    }
}
