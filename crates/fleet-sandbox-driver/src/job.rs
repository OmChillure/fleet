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
    /// restrict file writes/edits; other tokens match as substrings
    /// (`fleet-wsexec`, `read`, …). Bare `bash`/`sh` is always denied.
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
        let j: Job =
            serde_json::from_str(r#"{"repo_url":"https://github.com/x/y.git","prompt":"hi"}"#)
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
    fn deserializes_provider_omitted_npm_and_defaults() {
        let j: Job = serde_json::from_value(serde_json::json!({
            "model": "acme/m1",
            "provider": {
                "id": "acme",
                "base_url": "https://api.acme/v1",
                "models": { "m1": { "name": "M1" } }
            }
        }))
        .unwrap();
        assert_eq!(j.model.as_deref(), Some("acme/m1"));
        let p = j.provider.unwrap();
        assert_eq!(p.id, "acme");
        assert_eq!(p.npm, "@ai-sdk/openai-compatible");
        assert_eq!(p.base_url.as_deref(), Some("https://api.acme/v1"));
        assert_eq!(p.name, "Custom provider");
        assert_eq!(p.api_key_env, "OPENCODE_API_KEY");
        assert_eq!(p.models["m1"].name.as_deref(), Some("M1"));
    }

    #[test]
    fn provider_missing_id_rejects_job_json() {
        let err = serde_json::from_value::<Job>(serde_json::json!({
            "provider": {
                "base_url": "https://api.acme/v1",
                "models": { "m1": { "name": "M1" } }
            }
        }));
        assert!(err.is_err());
    }

    #[test]
    fn model_entry_flatten_extra_survives_deserialize() {
        let j: Job = serde_json::from_value(serde_json::json!({
            "provider": {
                "id": "acme",
                "models": { "m1": { "name": "M1", "limit": 128 } }
            }
        }))
        .unwrap();
        let m = &j.provider.unwrap().models["m1"];
        assert_eq!(m.name.as_deref(), Some("M1"));
        assert_eq!(m.extra.get("limit"), Some(&serde_json::json!(128)));
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
            harness_command: None,
            repo_url: None,
            git_ref: None,
            work_dir: None,
            analysis: Some("hello".into()),
            error: None,
            acp: None,
            opencode: None,
            provider_probe: None,
            provision: None,
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["job_id"], "j1");
        assert_eq!(v["type"], "analyze_repo");
        assert_eq!(v["ok"], true);
        assert_eq!(v["analysis"], "hello");
        assert_eq!(v["harness"], "opencode");
        assert!(v.get("error").is_none());
        assert!(v.get("opencode").is_none());
    }

    #[test]
    fn result_failed_shape_has_ok_false_and_error() {
        let r = JobResult {
            job_id: "j1".into(),
            r#type: "analyze_repo".into(),
            ok: false,
            started_at: "t0".into(),
            finished_at: "t1".into(),
            elapsed_sec: 1.5,
            harness: None,
            harness_command: None,
            repo_url: None,
            git_ref: None,
            work_dir: None,
            analysis: None,
            error: Some("boom".into()),
            acp: None,
            opencode: None,
            provider_probe: None,
            provision: None,
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"], "boom");
        assert!(v.get("analysis").is_none());
    }

    #[test]
    fn result_opencode_back_compat_alias_emitted_with_acp() {
        let blob = serde_json::json!({"protocol": "acp"});
        let r = JobResult {
            job_id: "j1".into(),
            r#type: "analyze_repo".into(),
            ok: true,
            started_at: "t0".into(),
            finished_at: "t1".into(),
            elapsed_sec: 1.5,
            harness: Some("opencode".into()),
            harness_command: None,
            repo_url: None,
            git_ref: None,
            work_dir: None,
            analysis: None,
            error: None,
            acp: Some(blob.clone()),
            opencode: Some(blob.clone()),
            provider_probe: None,
            provision: None,
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["acp"], blob);
        assert_eq!(v["opencode"], blob);
        assert_eq!(v.get("acp"), v.get("opencode"));
    }

    #[test]
    fn deserializes_project_setup() {
        let j: Job = serde_json::from_value(serde_json::json!({
            "project_setup": {
                "strategy": "detect",
                "commands": ["python -m pip install -r requirements.txt"],
                "smoke_commands": ["python manage.py check"]
            }
        }))
        .unwrap();
        let ps = j.project_setup.unwrap();
        assert_eq!(ps.strategy, "detect");
        assert_eq!(
            ps.commands,
            vec!["python -m pip install -r requirements.txt"]
        );
        assert_eq!(ps.smoke_commands, vec!["python manage.py check"]);
    }

    #[test]
    fn project_setup_omitted_strategy_defaults_to_commands() {
        let j: Job = serde_json::from_value(serde_json::json!({
            "project_setup": {
                "commands": ["pip install -r requirements.txt"]
            }
        }))
        .unwrap();
        let ps = j.project_setup.unwrap();
        assert_eq!(ps.strategy, "commands");
        assert_eq!(ps.commands, vec!["pip install -r requirements.txt"]);
    }

    #[test]
    fn project_setup_script_strategy_deserializes_from_job_json() {
        let j: Job = serde_json::from_value(serde_json::json!({
            "project_setup": {
                "strategy": "script",
                "script": "scripts/fleet-setup.sh"
            }
        }))
        .unwrap();
        let ps = j.project_setup.unwrap();
        assert_eq!(ps.strategy, "script");
        assert_eq!(ps.script.as_deref(), Some("scripts/fleet-setup.sh"));
    }

    #[test]
    fn project_setup_env_and_timeout_sec_from_json() {
        let j: Job = serde_json::from_value(serde_json::json!({
            "project_setup": {
                "env": {"PIP_DISABLE_PIP_VERSION_CHECK": "1"},
                "timeout_sec": 180
            }
        }))
        .unwrap();
        let ps = j.project_setup.unwrap();
        assert_eq!(ps.env.get("PIP_DISABLE_PIP_VERSION_CHECK").map(String::as_str), Some("1"));
        assert_eq!(ps.timeout_sec, Some(180));
    }
}
