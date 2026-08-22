//! Pi coding agent config: `~/.pi/agent/models.json` for local / OpenAI-compatible providers.
//! See https://pi.dev and packages/coding-agent docs/models.md

use crate::job::{Job, ProviderSpec};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use tracing::{info, warn};

/// Write Pi models.json (+ minimal settings) so pi-acp can use local LM Studio / Ollama / etc.
pub fn setup_pi(
    job: &Job,
    _work_dir: &Path,
    child_env: &mut HashMap<String, String>,
) -> Result<()> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/fleet".into());
    let agent_dir = Path::new(&home).join(".pi/agent");
    fs::create_dir_all(&agent_dir).with_context(|| format!("create {}", agent_dir.display()))?;

    let models = build_pi_models_json(job, child_env)?;
    let models_path = agent_dir.join("models.json");
    fs::write(&models_path, format!("{}\n", serde_json::to_string_pretty(&models)?))?;
    info!(path = %models_path.display(), "wrote Pi models.json");

    // Quiet ACP startup banners when possible
    let settings_path = agent_dir.join("settings.json");
    if !settings_path.is_file() {
        let settings = json!({ "quietStartup": true });
        fs::write(
            &settings_path,
            format!("{}\n", serde_json::to_string_pretty(&settings)?),
        )?;
    }

    if let Some(url) = models
        .pointer("/providers")
        .and_then(|p| p.as_object())
        .and_then(|m| m.values().next())
        .and_then(|p| p.get("baseUrl"))
        .and_then(|u| u.as_str())
    {
        match crate::opencode_config::probe_models_public(url) {
            Ok(ids) => {
                info!(url = %url, sample = ?ids.iter().take(6).collect::<Vec<_>>(), "pi provider /models OK");
                child_env.insert(
                    "_FLEET_PROVIDER_PROBE".into(),
                    serde_json::to_string(&json!({"ok": true, "url": url, "models": ids}))?,
                );
            }
            Err(e) => {
                warn!(url = %url, error = %e, "pi provider /models probe failed");
                child_env.insert(
                    "_FLEET_PROVIDER_PROBE".into(),
                    serde_json::to_string(&json!({"ok": false, "url": url, "error": e.to_string()}))?,
                );
            }
        }
    }

    Ok(())
}

pub(crate) fn build_pi_models_json(
    job: &Job,
    child_env: &mut HashMap<String, String>,
) -> Result<Value> {
    // Prefer job.provider (same ProviderSpec as OpenCode jobs), else env (OPENCODE_* / PI_*).
    if let Some(spec) = job.provider.clone().or_else(provider_from_env) {
        inject_pi_key(&spec, child_env);
        return Ok(models_from_provider(&spec, job.model.as_deref()));
    }

    warn!("no Pi provider config; relying on pre-existing ~/.pi/agent/models.json in image");
    Ok(json!({ "providers": {} }))
}

fn provider_from_env() -> Option<ProviderSpec> {
    let base = std::env::var("OPENCODE_BASE_URL")
        .or_else(|_| std::env::var("PI_BASE_URL"))
        .or_else(|_| std::env::var("LMSTUDIO_BASE_URL"))
        .ok()
        .filter(|s| !s.is_empty())?;
    let id = std::env::var("OPENCODE_PROVIDER_ID")
        .or_else(|_| std::env::var("PI_PROVIDER_ID"))
        .unwrap_or_else(|_| "lmstudio".into());
    let name = std::env::var("OPENCODE_PROVIDER_NAME")
        .unwrap_or_else(|_| "LM Studio (local)".into());
    let api_key = std::env::var("OPENCODE_API_KEY")
        .or_else(|_| std::env::var("PI_API_KEY"))
        .ok();
    let mut models = std::collections::BTreeMap::new();
    if let Ok(model) = std::env::var("MODEL") {
        let mid = model
            .split_once('/')
            .map(|(_, m)| m.to_string())
            .unwrap_or(model);
        models.insert(
            mid.clone(),
            crate::job::ModelEntry {
                name: Some(mid),
                extra: Default::default(),
            },
        );
    }
    Some(ProviderSpec {
        id,
        name,
        npm: "@ai-sdk/openai-compatible".into(),
        base_url: Some(base),
        api_key_env: "OPENCODE_API_KEY".into(),
        api_key,
        models,
        headers: Default::default(),
    })
}

fn inject_pi_key(spec: &ProviderSpec, child_env: &mut HashMap<String, String>) {
    if let Some(k) = &spec.api_key {
        child_env.insert(spec.api_key_env.clone(), k.clone());
        child_env.insert("OPENCODE_API_KEY".into(), k.clone());
    } else if let Ok(k) = std::env::var(&spec.api_key_env) {
        child_env.insert(spec.api_key_env.clone(), k);
    } else if let Ok(k) = std::env::var("OPENCODE_API_KEY") {
        child_env.insert("OPENCODE_API_KEY".into(), k);
    }
}

fn models_from_provider(spec: &ProviderSpec, job_model: Option<&str>) -> Value {
    let base = spec
        .base_url
        .clone()
        .or_else(|| std::env::var("OPENCODE_BASE_URL").ok())
        .unwrap_or_else(|| "http://host.docker.internal:1234/v1".into());
    let api_key = spec
        .api_key
        .clone()
        .or_else(|| std::env::var(&spec.api_key_env).ok())
        .or_else(|| std::env::var("OPENCODE_API_KEY").ok())
        .unwrap_or_else(|| "lm-studio".into());

    let mut model_list: Vec<Value> = Vec::new();
    for (id, entry) in &spec.models {
        model_list.push(json!({
            "id": id,
            "name": entry.name.as_deref().unwrap_or(id),
            "reasoning": false,
            "input": ["text"],
            "contextWindow": 128000,
            "maxTokens": 8192,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }
        }));
    }
    let env_model = std::env::var("MODEL").ok();
    if let Some(m) = job_model.or(env_model.as_deref()) {
        let mid = m.split_once('/').map(|(_, x)| x).unwrap_or(m);
        if !model_list
            .iter()
            .any(|v| v.get("id").and_then(|i| i.as_str()) == Some(mid))
        {
            model_list.push(json!({
                "id": mid,
                "name": mid,
                "reasoning": false,
                "input": ["text"],
                "contextWindow": 128000,
                "maxTokens": 8192,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }
            }));
        }
    }
    if model_list.is_empty() {
        model_list.push(json!({
            "id": "local-model",
            "name": "Local model",
            "reasoning": false,
            "input": ["text"],
            "contextWindow": 128000,
            "maxTokens": 8192,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }
        }));
    }

    json!({
        "providers": {
            spec.id.clone(): {
                "baseUrl": base,
                "api": "openai-completions",
                "apiKey": api_key,
                "compat": {
                    "supportsDeveloperRole": false,
                    "supportsReasoningEffort": false
                },
                "models": model_list
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_lmstudio_shape() {
        let job: Job = serde_json::from_value(serde_json::json!({
            "harness": "pi",
            "repo_url": "https://example.com/r.git",
            "model": "lmstudio/google/gemma-4-e4b",
            "provider": {
                "id": "lmstudio",
                "name": "LM Studio",
                "base_url": "http://host.docker.internal:1234/v1",
                "api_key": "lm-studio",
                "models": { "google/gemma-4-e4b": { "name": "Gemma" } }
            }
        }))
        .unwrap();
        let mut env = HashMap::new();
        let v = build_pi_models_json(&job, &mut env).unwrap();
        assert_eq!(
            v["providers"]["lmstudio"]["baseUrl"],
            "http://host.docker.internal:1234/v1"
        );
        assert_eq!(v["providers"]["lmstudio"]["api"], "openai-completions");
        let models = v["providers"]["lmstudio"]["models"].as_array().unwrap();
        assert!(models.iter().any(|m| m["id"] == "google/gemma-4-e4b"));
    }
}
