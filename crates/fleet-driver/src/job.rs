//! Job / result shapes for the lab bus (Redis).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Work unit pulled from `fleet:jobs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    #[serde(default = "new_id")]
    pub job_id: String,
    #[serde(default = "default_type")]
    pub r#type: String,
    /// `opencode` | `codex` | `grok` | `custom`
    #[serde(default = "default_harness")]
    pub harness: String,
    pub repo_url: Option<String>,
    #[serde(default = "default_ref")]
    pub git_ref: String,
    pub prompt: Option<String>,
    /// OpenCode model id: `provider_id/model_id` (docs).
    pub model: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout_sec: u64,
    pub work_dir: Option<String>,
    /// When true, approve all ACP permission requests (dangerous). Default **false**.
    #[serde(default)]
    pub auto_approve: bool,
    /// Permission / tool allowlist. Empty = deny all (unless auto_approve).
    /// Use `"*"` to allow everything. Path-like entries (`app/`, `tests/`)
    /// restrict file writes/edits; other tokens match as substrings (`bash`,
    /// `fleet-wsexec`, `read`, …).
    #[serde(default)]
    pub permission_allow: Vec<String>,

    /// Non-secret env injected into the sandbox process.
    #[serde(default)]
    pub env_public: BTreeMap<String, String>,
    /// Full `opencode.json` body (takes precedence for OpenCode setup).
    /// See https://opencode.ai/docs/config/ and /docs/providers/
    #[serde(default)]
    pub opencode: Option<Value>,
    /// Path inside container to an opencode.json to copy (optional).
    pub opencode_config_path: Option<String>,
    /// Simplified custom OpenAI-compatible provider (if `opencode` not set).
    #[serde(default)]
    pub provider: Option<ProviderSpec>,
    /// Custom harness only.
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<Value>,
    /// Optional secrets for harness auth (prefer env mounts in production).
    pub openai_api_key: Option<String>,
    pub openai_base_url: Option<String>,
    pub xai_api_key: Option<String>,
    pub anthropic_api_key: Option<String>,
    /// Back-compat aliases (mapped into ProviderSpec when provider is absent).
    pub lmstudio_base_url: Option<String>,
    pub lmstudio_api_key: Option<String>,
    /// Project env setup (runs in **workspace** container, not harness image).
    #[serde(default)]
    pub project_setup: Option<ProjectSetup>,
}

/// How to prepare the app environment on the workspace volume/container.
/// See docs/architecture/sandbox-workspace.md
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectSetup {
    /// `commands` | `script` | `detect` (detect reserved for future)
    #[serde(default = "default_setup_strategy")]
    pub strategy: String,
    /// Optional workspace image hint for orchestrator (compose already pins lab image).
    pub workspace_image: Option<String>,
    /// Shell commands run in work_dir after clone (workspace container).
    #[serde(default)]
    pub commands: Vec<String>,
    /// Optional smoke commands after deps install.
    #[serde(default)]
    pub smoke_commands: Vec<String>,
    /// In-repo script path relative to work_dir (strategy=script).
    pub script: Option<String>,
    /// Extra env for setup commands only.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Seconds to wait for workspace agent (default 900).
    pub timeout_sec: Option<u64>,
    /// Named workspace tools (per project). Harness may only invoke these by name
    /// via `fleet-wsexec <tool>` — no free shell into the workspace.
    #[serde(default)]
    pub tools: BTreeMap<String, WorkspaceTool>,
}

/// Fixed command exposed to the harness as a named tool (allowlisted).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceTool {
    /// Exact argv run in the workspace container (cwd = job work_dir).
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

fn default_setup_strategy() -> String {
    "commands".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSpec {
    /// Key under `provider` in opencode.json (e.g. `openai`, `myprovider`, `lmstudio`).
    pub id: String,
    #[serde(default = "default_provider_name")]
    pub name: String,
    /// AI SDK package; custom OpenAI-compatible → `@ai-sdk/openai-compatible`.
    #[serde(default = "default_npm")]
    pub npm: String,
    pub base_url: Option<String>,
    /// Env var name for apiKey via `{env:VAR}` (OpenCode docs).
    #[serde(default = "default_api_key_env")]
    pub api_key_env: String,
    /// Literal api key if not using env (lab only).
    pub api_key: Option<String>,
    /// Models map keys = model ids (without provider prefix).
    #[serde(default)]
    pub models: BTreeMap<String, ModelEntry>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    pub name: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobResult {
    pub job_id: String,
    pub r#type: String,
    pub ok: bool,
    pub started_at: String,
    pub finished_at: String,
    pub elapsed_sec: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness_command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analysis: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acp: Option<Value>,
    /// Back-compat for wait-result scripts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opencode: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_probe: Option<Value>,
    /// Provision milestones / workspace agent result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provision: Option<Value>,
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn default_type() -> String {
    "analyze_repo".into()
}
fn default_harness() -> String {
    "opencode".into()
}
fn default_ref() -> String {
    "main".into()
}
fn default_timeout() -> u64 {
    600
}
fn default_npm() -> String {
    "@ai-sdk/openai-compatible".into()
}
fn default_api_key_env() -> String {
    "OPENCODE_API_KEY".into()
}
fn default_provider_name() -> String {
    "Custom provider".into()
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_minimal_job() {
        let j: Job = serde_json::from_str(
            r#"{"repo_url":"https://github.com/x/y.git","prompt":"hi"}"#,
        )
        .unwrap();
        assert_eq!(j.harness, "opencode");
        assert_eq!(j.r#type, "analyze_repo");
        assert!(!j.auto_approve, "permissions default deny");
        assert!(j.permission_allow.is_empty());
        assert_eq!(j.timeout_sec, 600);
        assert!(!j.job_id.is_empty());
        assert_eq!(j.repo_url.as_deref(), Some("https://github.com/x/y.git"));
    }

    #[test]
    fn deserializes_custom_harness_and_provider() {
        let j: Job = serde_json::from_value(serde_json::json!({
            "harness": "custom",
            "command": "my-agent",
            "args": ["stdio"],
            "model": "acme/m1",
            "provider": {
                "id": "acme",
                "base_url": "https://api.acme/v1",
                "models": { "m1": { "name": "M1" } }
            }
        }))
        .unwrap();
        assert_eq!(j.harness, "custom");
        assert_eq!(j.command.as_deref(), Some("my-agent"));
        assert_eq!(j.args, vec!["stdio"]);
        let p = j.provider.unwrap();
        assert_eq!(p.id, "acme");
        assert_eq!(p.npm, "@ai-sdk/openai-compatible");
        assert!(p.models.contains_key("m1"));
    }

    #[test]
    fn result_serializes_ok_shape() {
        let r = JobResult {
            job_id: "j1".into(),
            r#type: "analyze_repo".into(),
            ok: true,
            started_at: "t0".into(),
            finished_at: "t1".into(),
            elapsed_sec: 1.5,
            harness: Some("opencode".into()),
            harness_command: Some(vec!["opencode".into(), "acp".into()]),
            repo_url: Some("https://x".into()),
            git_ref: Some("main".into()),
            work_dir: Some("/workspace/repo".into()),
            analysis: Some("hello".into()),
            error: None,
            acp: Some(serde_json::json!({"protocol":"acp"})),
            opencode: None,
            provider_probe: None,
            provision: None,
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["analysis"], "hello");
        assert_eq!(v["harness"], "opencode");
        assert!(v.get("error").is_none() || v["error"].is_null());
    }

    #[test]
    fn deserializes_project_setup() {
        let j: Job = serde_json::from_value(serde_json::json!({
            "repo_url": "https://github.com/eugenepyvovarov/123-invoices.git",
            "project_setup": {
                "strategy": "commands",
                "workspace_image": "python:3.13-bookworm",
                "commands": ["python -m pip install -r requirements.txt"],
                "smoke_commands": ["python manage.py check"]
            }
        }))
        .unwrap();
        let ps = j.project_setup.unwrap();
        assert_eq!(ps.strategy, "commands");
        assert_eq!(ps.commands.len(), 1);
        assert_eq!(ps.smoke_commands.len(), 1);
    }

    #[test]
    fn deserializes_workspace_tools() {
        let j: Job = serde_json::from_value(serde_json::json!({
            "repo_url": "https://example.com/r.git",
            "project_setup": {
                "tools": {
                    "django_check": {
                        "argv": ["python", "manage.py", "check"],
                        "description": "Django system check"
                    }
                }
            }
        }))
        .unwrap();
        let t = j.project_setup.unwrap().tools;
        assert_eq!(t["django_check"].argv, vec!["python", "manage.py", "check"]);
    }
}
