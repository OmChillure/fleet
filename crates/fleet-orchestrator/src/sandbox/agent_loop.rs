//! Worker LLM *selection* for spawn (`FLEET_HARNESS` / model).
//!
//! The control plane is not a runner. Completions run on a paired
//! `fleet-host` via the official CLI. This module must not call xAI/Ollama.

/// Resolved worker LLM after combining env (Grok only) + workspace settings (Ollama).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerLlm {
    pub backend: Option<String>,
    pub model: Option<String>,
}

impl WorkerLlm {
    pub fn is_llm(&self) -> bool {
        self.backend.is_some()
    }
}

/// `FLEET_AGENT_BACKEND=grok` — host/env only, never a Settings value.
pub fn grok_env_enabled() -> bool {
    if cfg!(test) {
        return false;
    }
    matches!(
        std::env::var("FLEET_AGENT_BACKEND")
            .ok()
            .map(|s| s.trim().to_ascii_lowercase())
            .as_deref(),
        Some("grok" | "xai")
    )
}

fn grok_model() -> Option<String> {
    std::env::var("GROK_MODEL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Normalize chat/plan provider ids to a worker backend.
pub fn normalize_provider(provider: &str) -> Option<String> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "" | "off" | "none" | "bash" => None,
        "grok" | "xai" => Some("grok".into()),
        "ollama" => Some("ollama".into()),
        "claude" | "anthropic" => Some("claude".into()),
        other => Some(other.to_string()),
    }
}

/// Workspace Settings / env only (no chat turn).
pub fn resolve_workspace_llm(fleet_llm: &str, fleet_llm_model: &str) -> WorkerLlm {
    resolve_worker_llm(None, None, fleet_llm, fleet_llm_model)
}

/// Plan/chat selection wins (same model as the composer). Then settings Ollama, then env Grok.
pub fn resolve_worker_llm(
    plan_provider: Option<&str>,
    plan_model: Option<&str>,
    fleet_llm: &str,
    fleet_llm_model: &str,
) -> WorkerLlm {
    if let Some(backend) = plan_provider.and_then(normalize_provider) {
        let model = plan_model
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| match backend.as_str() {
                "grok" => grok_model(),
                "ollama" => std::env::var("OLLAMA_MODEL")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
                _ => None,
            });
        return WorkerLlm {
            backend: Some(backend),
            model,
        };
    }
    if grok_env_enabled() {
        return WorkerLlm {
            backend: Some("grok".into()),
            model: grok_model(),
        };
    }
    let llm = fleet_llm.trim().to_ascii_lowercase();
    if llm == "ollama" {
        let model = fleet_llm_model.trim().to_string();
        let model = if model.is_empty() {
            std::env::var("OLLAMA_MODEL")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        } else {
            Some(model)
        };
        return WorkerLlm {
            backend: Some("ollama".into()),
            model,
        };
    }
    WorkerLlm {
        backend: None,
        model: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_off_is_bash_only_in_tests() {
        let w = resolve_worker_llm(None, None, "off", "");
        assert!(w.backend.is_none());
    }

    #[test]
    fn settings_ollama_picks_model() {
        let w = resolve_worker_llm(None, None, "ollama", "qwen2.5-coder");
        assert_eq!(w.backend.as_deref(), Some("ollama"));
        assert_eq!(w.model.as_deref(), Some("qwen2.5-coder"));
    }

    #[test]
    fn chat_selection_wins_over_settings() {
        let w = resolve_worker_llm(
            Some("grok"),
            Some("grok-4.5-internal"),
            "ollama",
            "llama3.2",
        );
        assert_eq!(w.backend.as_deref(), Some("grok"));
        assert_eq!(w.model.as_deref(), Some("grok-4.5-internal"));
    }

    #[test]
    fn future_claude_provider_is_preserved() {
        let w = resolve_worker_llm(Some("claude"), Some("claude-opus-4"), "off", "");
        assert_eq!(w.backend.as_deref(), Some("claude"));
        assert_eq!(w.model.as_deref(), Some("claude-opus-4"));
    }

    #[test]
    fn plan_grok_wins_over_workspace_ollama_and_off() {
        let w = resolve_worker_llm(Some("grok"), Some("grok-4.5"), "ollama", "llama3.2");
        assert_eq!(
            w.backend.as_deref(),
            Some("grok"),
            "plan LLM is the product source of truth, not Settings / API env"
        );
        assert_eq!(w.model.as_deref(), Some("grok-4.5"));
        let off = resolve_worker_llm(Some("grok"), Some("grok-4.5"), "off", "");
        assert_eq!(off.backend.as_deref(), Some("grok"));
    }

    #[test]
    fn normalize_provider_maps_aliases_and_off() {
        assert_eq!(normalize_provider(""), None);
        assert_eq!(normalize_provider("off"), None);
        assert_eq!(normalize_provider("none"), None);
        assert_eq!(normalize_provider("bash"), None);
        assert_eq!(normalize_provider("xai").as_deref(), Some("grok"));
        assert_eq!(normalize_provider("GROK").as_deref(), Some("grok"));
        assert_eq!(normalize_provider("ollama").as_deref(), Some("ollama"));
    }

    #[test]
    fn grok_env_is_forced_off_in_unit_tests() {
        assert!(!grok_env_enabled());
        let w = resolve_workspace_llm("off", "ignored");
        assert!(!w.is_llm());
    }
}
