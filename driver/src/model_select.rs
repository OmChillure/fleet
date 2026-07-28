//! Per-profile model selection helpers.
//! Model / thought lists come from ACP discovery only (no hardcoded catalogs).

use crate::models::HarnessKind;

pub fn label_for_option(model: &str) -> String {
    if model.is_empty() {
        "(agent default)".into()
    } else {
        model.to_string()
    }
}

/// Inject CLI model flags only where the harness ACP entrypoint accepts them.
///
/// **OpenCode:** do **not** put `-m` on the CLI. `opencode acp` has no `-m` flag;
/// `opencode -m MODEL acp` is mis-parsed (often prints ACP help and exits).
/// Model is applied only via ACP `session/set_config_option` after `session/new`.
///
/// **Grok:** `grok agent -m MODEL stdio` works.
/// **Codex:** optional `--model` on the adapter binary.
pub fn inject_model_args(
    harness: HarnessKind,
    args: &[String],
    model: Option<&str>,
) -> Vec<String> {
    let Some(model) = model.map(str::trim).filter(|m| !m.is_empty()) else {
        return args.to_vec();
    };

    // Strip any existing model flags first
    let stripped = strip_model_flags(args);

    match harness {
        HarnessKind::Grok => inject_after_token(&stripped, "agent", model),
        // OpenCode ACP is the `acp` subcommand only — no CLI model flag.
        HarnessKind::Opencode => stripped,
        HarnessKind::Codex => {
            // codex-acp / npx package: pass --model when possible
            let mut out = stripped;
            out.push("--model".into());
            out.push(model.to_string());
            out
        }
        HarnessKind::Custom => {
            // Best-effort: append --model (may break some binaries; prefer ACP)
            let mut out = stripped;
            out.push("--model".into());
            out.push(model.to_string());
            out
        }
    }
}

fn strip_model_flags(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "-m" || a == "--model" {
            i += 2; // skip flag + value
            continue;
        }
        if let Some(rest) = a.strip_prefix("--model=") {
            let _ = rest;
            i += 1;
            continue;
        }
        out.push(a.clone());
        i += 1;
    }
    out
}

/// Insert `-m MODEL` immediately after `token` (e.g. after `agent`).
fn inject_after_token(args: &[String], token: &str, model: &str) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len() + 2);
    let mut inserted = false;
    for a in args {
        out.push(a.clone());
        if !inserted && a == token {
            out.push("-m".into());
            out.push(model.to_string());
            inserted = true;
        }
    }
    if !inserted {
        // no agent token — prepend
        let mut prefixed = vec!["-m".into(), model.to_string()];
        prefixed.extend(out);
        return prefixed;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inject_grok_after_agent() {
        let args = vec!["agent".into(), "stdio".into()];
        let out = inject_model_args(HarnessKind::Grok, &args, Some("grok-4"));
        assert_eq!(out, vec!["agent", "-m", "grok-4", "stdio"]);
    }

    #[test]
    fn inject_replaces_existing_flag() {
        let args = vec!["agent".into(), "-m".into(), "old".into(), "stdio".into()];
        let out = inject_model_args(HarnessKind::Grok, &args, Some("new"));
        assert_eq!(out, vec!["agent", "-m", "new", "stdio"]);
    }

    #[test]
    fn inject_opencode_does_not_break_acp_subcommand() {
        let args = vec!["acp".into()];
        // Must stay exactly `opencode acp` — model goes via ACP protocol only.
        let out = inject_model_args(
            HarnessKind::Opencode,
            &args,
            Some("minimax-coding-plan/MiniMax-M3"),
        );
        assert_eq!(out, vec!["acp"]);
    }

    #[test]
    fn inject_opencode_strips_stale_cli_model_flags() {
        let args = vec![
            "-m".into(),
            "old/model".into(),
            "acp".into(),
        ];
        let out = inject_model_args(HarnessKind::Opencode, &args, Some("new/model"));
        assert_eq!(out, vec!["acp"]);
    }

    #[test]
    fn no_model_leaves_args() {
        let args = vec!["agent".into(), "stdio".into()];
        let out = inject_model_args(HarnessKind::Grok, &args, None);
        assert_eq!(out, args);
    }
}
