//! Resolve where a job runs and what network it gets.
//!
//! Looser place: `on_pc` > `in_container`.
//! Looser network: `open` > `allowlist` > `none`.
//! `inherit` means the runner's own network. An envelope tighter than
//! `open` replaces `inherit`, because the runner might be wide open.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPosture {
    pub run_mode: String,
    pub egress_mode: String,
    /// The request was looser than the repo (or the no-repo default) and
    /// still inside the workspace envelope.
    pub loosened: bool,
}

pub fn resolve_posture(
    run_envelope: &str,
    egress_envelope: &str,
    fallback_run: &str,
    fallback_egress: &str,
    repo_run: Option<&str>,
    repo_egress: Option<&str>,
    request_run: Option<&str>,
    request_egress: Option<&str>,
) -> Result<ResolvedPosture, &'static str> {
    let run_cap = run_rank(run_envelope).ok_or("run envelope must be on_pc or in_container")?;
    let egress_cap =
        egress_rank(egress_envelope).ok_or("egress envelope must be open, allowlist, or none")?;

    let base_run = concrete_run(repo_run).or_else(|| concrete_run(Some(fallback_run)));
    let base_run = base_run.ok_or("run mode must be on_pc or in_container")?;
    let base_egress = concrete_egress(repo_egress)
        .or_else(|| concrete_egress(Some(fallback_egress)))
        .unwrap_or("inherit");

    let mut loosened = false;
    let chosen_run = match concrete_run(request_run) {
        Some(asked) => {
            let asked_rank = run_rank(asked).unwrap();
            let base_rank = run_rank(base_run).unwrap();
            if asked_rank > run_cap {
                return Err("that place is looser than this workspace allows");
            }
            if asked_rank > base_rank {
                loosened = true;
            }
            asked
        }
        None => base_run,
    };
    let chosen_egress = match request_egress.map(str::trim).filter(|s| !s.is_empty()) {
        Some("inherit") | None => base_egress,
        Some(asked) => {
            let asked_rank = egress_rank(asked).ok_or("egress must be none, allowlist, or open")?;
            if asked_rank > egress_cap {
                return Err("that network is looser than this workspace allows");
            }
            if let Some(base_rank) = egress_rank(base_egress) {
                if asked_rank > base_rank {
                    loosened = true;
                }
            } else if asked_rank > 0 {
                loosened = true;
            }
            asked
        }
    };

    let run_mode = clip_run(chosen_run, run_cap).to_string();
    let egress_mode = clip_egress(chosen_egress, egress_cap).to_string();
    Ok(ResolvedPosture {
        run_mode,
        egress_mode,
        loosened,
    })
}

fn concrete_run(raw: Option<&str>) -> Option<&'static str> {
    raw.map(str::trim).filter(|s| !s.is_empty()).and_then(run_name)
}

fn concrete_egress(raw: Option<&str>) -> Option<&'static str> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        Some("inherit") | None => None,
        Some(other) => egress_name(other),
    }
}

fn run_name(raw: &str) -> Option<&'static str> {
    match raw {
        "on_pc" => Some("on_pc"),
        "in_container" => Some("in_container"),
        _ => None,
    }
}

fn egress_name(raw: &str) -> Option<&'static str> {
    match raw {
        "none" => Some("none"),
        "allowlist" => Some("allowlist"),
        "open" => Some("open"),
        _ => None,
    }
}

fn run_rank(raw: &str) -> Option<u8> {
    match raw.trim() {
        "in_container" => Some(0),
        "on_pc" => Some(1),
        _ => None,
    }
}

fn egress_rank(raw: &str) -> Option<u8> {
    match raw.trim() {
        "none" => Some(0),
        "allowlist" => Some(1),
        "open" => Some(2),
        _ => None,
    }
}

fn clip_run(mode: &str, cap: u8) -> &'static str {
    let rank = run_rank(mode).unwrap_or(0);
    if rank > cap {
        "in_container"
    } else {
        run_name(mode).unwrap_or("in_container")
    }
}

fn clip_egress(mode: &str, cap: u8) -> &'static str {
    if mode == "inherit" {
        return if cap < 2 {
            egress_name_from_rank(cap)
        } else {
            "inherit"
        };
    }
    let rank = egress_rank(mode).unwrap_or(0);
    if rank > cap {
        egress_name_from_rank(cap)
    } else {
        egress_name(mode).unwrap_or("none")
    }
}

fn egress_name_from_rank(rank: u8) -> &'static str {
    match rank {
        0 => "none",
        1 => "allowlist",
        _ => "open",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_default_is_clipped_by_the_workspace_cap() {
        let got = resolve_posture(
            "in_container",
            "none",
            "on_pc",
            "open",
            Some("on_pc"),
            Some("open"),
            None,
            None,
        )
        .unwrap();
        assert_eq!(got.run_mode, "in_container");
        assert_eq!(got.egress_mode, "none");
        assert!(!got.loosened);
    }

    #[test]
    fn user_may_tighten_without_being_an_override() {
        let got = resolve_posture(
            "on_pc",
            "open",
            "on_pc",
            "open",
            Some("on_pc"),
            Some("open"),
            Some("in_container"),
            Some("none"),
        )
        .unwrap();
        assert_eq!(got.run_mode, "in_container");
        assert_eq!(got.egress_mode, "none");
        assert!(!got.loosened);
    }

    #[test]
    fn user_may_loosen_inside_the_envelope() {
        let got = resolve_posture(
            "on_pc",
            "open",
            "in_container",
            "none",
            Some("in_container"),
            Some("none"),
            Some("on_pc"),
            Some("allowlist"),
        )
        .unwrap();
        assert_eq!(got.run_mode, "on_pc");
        assert_eq!(got.egress_mode, "allowlist");
        assert!(got.loosened);
    }

    #[test]
    fn user_may_not_loosen_past_the_envelope() {
        let err = resolve_posture(
            "in_container",
            "none",
            "in_container",
            "none",
            Some("in_container"),
            Some("none"),
            Some("on_pc"),
            None,
        )
        .unwrap_err();
        assert!(err.contains("workspace"));
    }

    #[test]
    fn no_repo_uses_the_workspace_default_then_the_cap() {
        let got = resolve_posture(
            "on_pc",
            "allowlist",
            "on_pc",
            "inherit",
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(got.run_mode, "on_pc");
        assert_eq!(got.egress_mode, "allowlist");
    }
}
