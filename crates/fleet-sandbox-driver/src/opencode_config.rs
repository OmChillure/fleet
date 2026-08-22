//! OpenCode config per https://opencode.ai/docs/config/ and /docs/providers/
//!
//! General provider setup — not LM Studio specific.
//! Custom OpenAI-compatible providers use:
//!   provider.<id>.npm = "@ai-sdk/openai-compatible"
//!   provider.<id>.options.baseURL / apiKey
//!   provider.<id>.models.<model_id>
//! Model ids are `provider_id/model_id`.

use crate::job::{Job, ModelEntry, ProviderSpec};
use anyhow::{Context, Result};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use tracing::{info, warn};

pub fn setup_opencode(
    job: &Job,
    _work_dir: &Path,
    child_env: &mut HashMap<String, String>,
) -> Result<()> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/fleet".into());
    let cfg_dir = Path::new(
        &std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| format!("{home}/.config")),
    )
    .join("opencode");
    fs::create_dir_all(&cfg_dir)?;
    let out = cfg_dir.join("opencode.json");

    let config = build_opencode_config(job, child_env)?;
    let text = serde_json::to_string_pretty(&config)?;
    fs::write(&out, format!("{text}\n"))?;
    info!(path = %out.display(), "wrote OpenCode config");

    if let Some(url) = extract_base_url(&config) {
        match probe_models(&url) {
            Ok(ids) => {
                info!(url = %url, sample = ?ids.iter().take(6).collect::<Vec<_>>(), "provider /models OK");
                // stash for caller via env marker is awkward; return via child_env private key
                child_env.insert(
                    "_FLEET_PROVIDER_PROBE".into(),
                    serde_json::to_string(&json!({"ok": true, "url": url, "models": ids}))?,
                );
            }
            Err(e) => {
                warn!(url = %url, error = %e, "provider /models probe failed");
                child_env.insert(
                    "_FLEET_PROVIDER_PROBE".into(),
                    serde_json::to_string(&json!({"ok": false, "url": url, "error": e.to_string()}))?,
                );
            }
        }
    }

    Ok(())
}

pub(crate) fn build_opencode_config(job: &Job, child_env: &mut HashMap<String, String>) -> Result<Value> {
    // 1) Explicit full config object (highest priority)
    if let Some(cfg) = &job.opencode {
        inject_api_keys_from_job(job, child_env);
        return Ok(cfg.clone());
    }

    // 2) Copy from path
    if let Some(path) = &job.opencode_config_path {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("read opencode_config_path {path}"))?;
        inject_api_keys_from_job(job, child_env);
        return Ok(serde_json::from_str(&raw)?);
    }

    // 3) ProviderSpec or env-driven custom / built-in
    if let Some(spec) = resolve_provider_spec(job) {
        inject_provider_key(&spec, job, child_env);
        return Ok(config_from_provider_spec(&spec, job.model.as_deref()));
    }

    // 4) Built-in providers via model prefix only (openai/…, anthropic/…)
    //    Write minimal config with apiKey env substitution when keys present.
    let model_owned = job
        .model
        .clone()
        .or_else(|| std::env::var("MODEL").ok())
        .or_else(|| std::env::var("OPENCODE_MODEL").ok());
    if let Some(model) = model_owned {
        inject_api_keys_from_job(job, child_env);
        if let Some((prov, _)) = model.split_once('/') {
            return Ok(minimal_builtin_config(prov, &model, child_env));
        }
    }

    // 5) Empty shell — OpenCode may still use preloaded providers + auth.json
    warn!("no OpenCode provider config supplied; relying on image defaults / auth.json");
    inject_api_keys_from_job(job, child_env);
    Ok(json!({
        "$schema": "https://opencode.ai/config.json"
    }))
}

pub(crate) fn resolve_provider_spec(job: &Job) -> Option<ProviderSpec> {
    if let Some(p) = &job.provider {
        return Some(p.clone());
    }

    // Env-driven custom OpenAI-compatible provider
    let id = std::env::var("OPENCODE_PROVIDER_ID").ok().filter(|s| !s.is_empty());
    let base = std::env::var("OPENCODE_BASE_URL")
        .ok()
        .or_else(|| std::env::var("PROVIDER_BASE_URL").ok())
        // back-compat
        .or_else(|| job.lmstudio_base_url.clone())
        .or_else(|| std::env::var("LMSTUDIO_BASE_URL").ok());

    if let (Some(id), Some(base_url)) = (id, base.clone()) {
        let model = job
            .model
            .clone()
            .or_else(|| std::env::var("MODEL").ok())
            .or_else(|| std::env::var("OPENCODE_MODEL").ok());
        let mut models = std::collections::BTreeMap::new();
        if let Some(m) = &model {
            let model_id = m
                .strip_prefix(&format!("{id}/"))
                .unwrap_or(m)
                .to_string();
            models.insert(
                model_id.clone(),
                ModelEntry {
                    name: Some(model_id),
                    extra: Default::default(),
                },
            );
        }
        return Some(ProviderSpec {
            id: id.clone(),
            name: std::env::var("OPENCODE_PROVIDER_NAME").unwrap_or_else(|_| id.clone()),
            npm: std::env::var("OPENCODE_PROVIDER_NPM")
                .unwrap_or_else(|_| "@ai-sdk/openai-compatible".into()),
            base_url: Some(base_url),
            api_key_env: std::env::var("OPENCODE_API_KEY_ENV")
                .unwrap_or_else(|_| "OPENCODE_API_KEY".into()),
            api_key: job
                .lmstudio_api_key
                .clone()
                .or_else(|| std::env::var("OPENCODE_API_KEY").ok())
                .or_else(|| std::env::var("LMSTUDIO_API_KEY").ok()),
            models,
            headers: Default::default(),
        });
    }

    // Back-compat: lmstudio_* fields alone → provider id lmstudio
    if let Some(base_url) = job
        .lmstudio_base_url
        .clone()
        .or_else(|| std::env::var("LMSTUDIO_BASE_URL").ok())
    {
        let model = job
            .model
            .clone()
            .or_else(|| std::env::var("LMSTUDIO_MODEL").ok())
            .or_else(|| std::env::var("MODEL").ok())
            .unwrap_or_else(|| "lmstudio/model".into());
        let model_id = model
            .strip_prefix("lmstudio/")
            .unwrap_or(&model)
            .to_string();
        let mut models = std::collections::BTreeMap::new();
        models.insert(
            model_id.clone(),
            ModelEntry {
                name: Some(model_id.clone()),
                extra: Default::default(),
            },
        );
        return Some(ProviderSpec {
            id: "lmstudio".into(),
            name: "LM Studio (local)".into(),
            npm: "@ai-sdk/openai-compatible".into(),
            base_url: Some(base_url),
            api_key_env: "OPENCODE_API_KEY".into(),
            api_key: job
                .lmstudio_api_key
                .clone()
                .or_else(|| std::env::var("LMSTUDIO_API_KEY").ok())
                .or(Some("lm-studio".into())),
            models,
            headers: Default::default(),
        });
    }

    None
}

pub(crate) fn config_from_provider_spec(spec: &ProviderSpec, model: Option<&str>) -> Value {
    let mut models_obj = Map::new();
    for (id, entry) in &spec.models {
        let mut m = Map::new();
        m.insert(
            "name".into(),
            json!(entry.name.clone().unwrap_or_else(|| id.clone())),
        );
        for (k, v) in &entry.extra {
            m.insert(k.clone(), v.clone());
        }
        models_obj.insert(id.clone(), Value::Object(m));
    }
    if let Some(m) = model {
        let mid = m
            .strip_prefix(&format!("{}/", spec.id))
            .unwrap_or(m)
            .to_string();
        models_obj.entry(mid.clone()).or_insert_with(|| {
            json!({ "name": mid })
        });
    }

    let mut options = Map::new();
    if let Some(url) = &spec.base_url {
        options.insert("baseURL".into(), json!(url));
    }
    // Prefer OpenCode {env:VAR} substitution (docs)
    options.insert(
        "apiKey".into(),
        json!(format!("{{env:{}}}", spec.api_key_env)),
    );
    if !spec.headers.is_empty() {
        options.insert("headers".into(), json!(spec.headers));
    }

    let mut provider_body = Map::new();
    // Built-in providers may omit npm; custom openai-compatible need it (docs).
    let builtin = matches!(
        spec.id.as_str(),
        "openai" | "anthropic" | "google" | "gemini" | "azure" | "amazon-bedrock" | "opencode"
    );
    if !builtin || spec.npm != "@ai-sdk/openai-compatible" {
        // Always set npm for custom ids; for builtin only if explicitly non-default path
        if !builtin {
            provider_body.insert("npm".into(), json!(spec.npm));
        }
    }
    if !builtin {
        provider_body.insert("npm".into(), json!(spec.npm));
        provider_body.insert("name".into(), json!(spec.name));
    } else if !spec.name.is_empty() && spec.name != "Custom provider" {
        provider_body.insert("name".into(), json!(spec.name));
    }
    provider_body.insert("options".into(), Value::Object(options));
    if !models_obj.is_empty() {
        provider_body.insert("models".into(), Value::Object(models_obj));
    }

    let mut provider = Map::new();
    provider.insert(spec.id.clone(), Value::Object(provider_body));

    let full_model = model.map(|m| {
        if m.contains('/') {
            m.to_string()
        } else {
            format!("{}/{}", spec.id, m)
        }
    });

    let mut root = Map::new();
    root.insert("$schema".into(), json!("https://opencode.ai/config.json"));
    if let Some(m) = full_model {
        root.insert("model".into(), json!(m));
    }
    root.insert("provider".into(), Value::Object(provider));
    Value::Object(root)
}

fn minimal_builtin_config(prov: &str, model: &str, child_env: &HashMap<String, String>) -> Value {
    let key_env = match prov {
        "openai" => "OPENAI_API_KEY",
        "anthropic" => "ANTHROPIC_API_KEY",
        "google" | "gemini" => "GOOGLE_API_KEY",
        _ => "OPENCODE_API_KEY",
    };
    // Only add options if we actually have a key in env/child
    let has_key = child_env.contains_key(key_env) || std::env::var(key_env).is_ok();
    if has_key {
        json!({
            "$schema": "https://opencode.ai/config.json",
            "model": model,
            "provider": {
                prov: {
                    "options": {
                        "apiKey": format!("{{env:{key_env}}}")
                    }
                }
            }
        })
    } else {
        json!({
            "$schema": "https://opencode.ai/config.json",
            "model": model
        })
    }
}

fn inject_provider_key(
    spec: &ProviderSpec,
    job: &Job,
    child_env: &mut HashMap<String, String>,
) {
    if let Some(k) = &spec.api_key {
        child_env.insert(spec.api_key_env.clone(), k.clone());
    } else if let Ok(k) = std::env::var(&spec.api_key_env) {
        child_env.insert(spec.api_key_env.clone(), k);
    } else if let Some(k) = &job.lmstudio_api_key {
        child_env.insert(spec.api_key_env.clone(), k.clone());
    }
    inject_api_keys_from_job(job, child_env);
}

fn inject_api_keys_from_job(job: &Job, child_env: &mut HashMap<String, String>) {
    if let Some(k) = &job.openai_api_key {
        child_env.insert("OPENAI_API_KEY".into(), k.clone());
    }
    if let Some(k) = &job.anthropic_api_key {
        child_env.insert("ANTHROPIC_API_KEY".into(), k.clone());
    }
    if let Some(k) = &job.xai_api_key {
        child_env.insert("XAI_API_KEY".into(), k.clone());
    }
    // Pass through common keys already in process env
    for key in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENCODE_API_KEY",
        "GOOGLE_API_KEY",
        "XAI_API_KEY",
    ] {
        if !child_env.contains_key(key) {
            if let Ok(v) = std::env::var(key) {
                child_env.insert(key.into(), v);
            }
        }
    }
}

pub(crate) fn extract_base_url(config: &Value) -> Option<String> {
    let providers = config.get("provider")?.as_object()?;
    for (_id, body) in providers {
        if let Some(url) = body.pointer("/options/baseURL").and_then(|v| v.as_str()) {
            return Some(url.to_string());
        }
    }
    None
}

/// Probe OpenAI-compatible `{base}/models` (used by OpenCode + Pi setup).
pub(crate) fn probe_models_public(base_url: &str) -> Result<Vec<String>> {
    probe_models(base_url)
}

fn probe_models(base_url: &str) -> Result<Vec<String>> {
    let url = base_url.trim_end_matches('/');
    let models_url = if url.ends_with("/v1") {
        format!("{url}/models")
    } else {
        format!("{url}/v1/models")
    };
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;
    // blocking in async context is bad — use sync only from setup which is sync today.
    let resp = client
        .get(&models_url)
        .header("Authorization", "Bearer probe")
        .send()
        .with_context(|| format!("GET {models_url}"))?;
    let body: Value = resp.json()?;
    let ids = body
        .get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    Ok(ids)
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::{Job, ModelEntry, ProviderSpec};
    use std::collections::BTreeMap;

    fn empty_job() -> Job {
        serde_json::from_value(serde_json::json!({
            "harness": "opencode",
            "repo_url": "https://example.com/r.git"
        }))
        .unwrap()
    }

    #[test]
    fn full_opencode_object_passthrough() {
        let mut job = empty_job();
        job.opencode = Some(json!({
            "$schema": "https://opencode.ai/config.json",
            "model": "anthropic/claude-sonnet-4-5",
            "provider": {
                "anthropic": {
                    "options": { "apiKey": "{env:ANTHROPIC_API_KEY}" }
                }
            }
        }));
        let mut env = HashMap::new();
        let cfg = build_opencode_config(&job, &mut env).unwrap();
        assert_eq!(cfg["model"], "anthropic/claude-sonnet-4-5");
        assert!(cfg["provider"]["anthropic"]["options"]["apiKey"].is_string());
    }

    #[test]
    fn provider_spec_builds_openai_compatible_shape() {
        let mut models = BTreeMap::new();
        models.insert(
            "my-model".into(),
            ModelEntry {
                name: Some("My Model".into()),
                extra: BTreeMap::new(),
            },
        );
        let spec = ProviderSpec {
            id: "acme".into(),
            name: "Acme LLM".into(),
            npm: "@ai-sdk/openai-compatible".into(),
            base_url: Some("https://api.acme.test/v1".into()),
            api_key_env: "ACME_KEY".into(),
            api_key: Some("secret".into()),
            models,
            headers: BTreeMap::new(),
        };
        let cfg = config_from_provider_spec(&spec, Some("acme/my-model"));
        assert_eq!(cfg["model"], "acme/my-model");
        assert_eq!(
            cfg["provider"]["acme"]["npm"],
            "@ai-sdk/openai-compatible"
        );
        assert_eq!(
            cfg["provider"]["acme"]["options"]["baseURL"],
            "https://api.acme.test/v1"
        );
        assert_eq!(
            cfg["provider"]["acme"]["options"]["apiKey"],
            "{env:ACME_KEY}"
        );
        assert_eq!(
            cfg["provider"]["acme"]["models"]["my-model"]["name"],
            "My Model"
        );
        assert_eq!(
            extract_base_url(&cfg).as_deref(),
            Some("https://api.acme.test/v1")
        );
    }

    #[test]
    fn model_without_prefix_gets_provider_prefix() {
        let mut models = BTreeMap::new();
        models.insert(
            "gemma".into(),
            ModelEntry {
                name: Some("Gemma".into()),
                extra: BTreeMap::new(),
            },
        );
        let spec = ProviderSpec {
            id: "local".into(),
            name: "Local".into(),
            npm: "@ai-sdk/openai-compatible".into(),
            base_url: Some("http://127.0.0.1:1234/v1".into()),
            api_key_env: "OPENCODE_API_KEY".into(),
            api_key: None,
            models,
            headers: BTreeMap::new(),
        };
        let cfg = config_from_provider_spec(&spec, Some("gemma"));
        assert_eq!(cfg["model"], "local/gemma");
    }

    #[test]
    fn job_provider_field_used_when_no_opencode_object() {
        let mut job = empty_job();
        job.model = Some("acme/my-model".into());
        job.provider = Some(ProviderSpec {
            id: "acme".into(),
            name: "Acme".into(),
            npm: "@ai-sdk/openai-compatible".into(),
            base_url: Some("https://x.test/v1".into()),
            api_key_env: "OPENCODE_API_KEY".into(),
            api_key: Some("k".into()),
            models: BTreeMap::new(),
            headers: BTreeMap::new(),
        });
        let mut env = HashMap::new();
        let cfg = build_opencode_config(&job, &mut env).unwrap();
        assert_eq!(cfg["provider"]["acme"]["options"]["baseURL"], "https://x.test/v1");
        assert_eq!(env.get("OPENCODE_API_KEY").map(String::as_str), Some("k"));
    }
}
