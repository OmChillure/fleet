//! Pure policy helpers for sandbox isolation and named tools.
//! Kept free of Redis/Docker I/O so unit tests can drive real shipped logic.

use crate::job::{ProjectSetup, WorkspaceTool};
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// Isolation root (lab default `/workspace/jobs`). Overridable via `FLEET_JOBS_ROOT`.
pub fn jobs_root() -> PathBuf {
    jobs_root_from(std::env::var("FLEET_JOBS_ROOT").ok().as_deref())
}

pub(crate) fn jobs_root_from(raw: Option<&str>) -> PathBuf {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/workspace/jobs"))
}

/// Default per-job source root under the shared volume.
pub fn default_job_work_dir(job_id: &str) -> PathBuf {
    jobs_root().join(job_id).join("repo")
}

/// Resolve work_dir: explicit job field or per-job default.
pub fn resolve_work_dir(job_id: &str, work_dir: Option<&str>) -> PathBuf {
    match work_dir {
        Some(p) if !p.trim().is_empty() => PathBuf::from(p),
        _ => default_job_work_dir(job_id),
    }
}

/// Job isolation root: parent of `.../repo` when path ends with `repo`, else the path itself.
pub fn job_root_for_work_dir(work_dir: &Path) -> PathBuf {
    if work_dir.file_name().and_then(|s| s.to_str()) == Some("repo") {
        work_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| work_dir.to_path_buf())
    } else {
        work_dir.to_path_buf()
    }
}

/// Reject path traversal and require job isolation under `/workspace/jobs/`.
pub fn validate_job_cwd(cwd: &Path) -> Result<()> {
    let s = cwd.to_string_lossy();
    if s.contains('\0') {
        bail!("cwd contains NUL");
    }
    for c in cwd.components() {
        if matches!(c, Component::ParentDir) {
            bail!("cwd must not contain '..': {}", cwd.display());
        }
    }
    // Normalize without requiring filesystem existence
    let normalized = cwd
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect::<PathBuf>();
    let ns = normalized.to_string_lossy();
    let root = jobs_root();
    let root_s = root.to_string_lossy().trim_end_matches('/').to_string();
    if ns != root_s.as_str() && !ns.starts_with(&(root_s.clone() + "/")) {
        bail!(
            "cwd outside job isolation (must be under {root_s}/): {}",
            cwd.display()
        );
    }
    Ok(())
}

/// Look up a named workspace tool; error if unknown or empty argv.
pub fn resolve_named_tool<'a>(
    tools: &'a BTreeMap<String, WorkspaceTool>,
    name: &str,
) -> Result<&'a WorkspaceTool> {
    let Some(tool) = tools.get(name) else {
        let allowed: Vec<_> = tools.keys().cloned().collect();
        bail!("unknown tool {name:?}; allowed={allowed:?}");
    };
    if tool.argv.is_empty() {
        bail!("tool {name:?} has empty argv");
    }
    for a in &tool.argv {
        if a.chars()
            .any(|c| matches!(c, '\n' | '\r' | ';' | '|' | '&' | '$' | '`'))
        {
            bail!("tool {name:?} argv rejected: {a:?}");
        }
    }
    validate_tool_bin(&tool.argv[0])
        .with_context(|| format!("tool {name:?} argv[0] not allowlisted"))?;
    Ok(tool)
}

/// Basenames permitted as named-tool argv[0]. No bash/sh/curl/wget/sudo.
pub fn validate_tool_bin(argv0: &str) -> Result<()> {
    let base = Path::new(argv0)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(argv0);
    const ALLOW: &[&str] = &[
        "python",
        "python3",
        "pip",
        "pip3",
        "npm",
        "npx",
        "node",
        "pytest",
        "cargo",
        "go",
        "make",
        "uv",
        "poetry",
        "django-admin",
        "git",
        "ls",
        "cat",
        "head",
        "echo",
        "true",
        "false",
        "test",
        "mkdir",
        "cp",
        "touch",
    ];
    if ALLOW.contains(&base) {
        return Ok(());
    }
    bail!("argv[0] {argv0:?} (basename {base:?}) is not an allowlisted tool binary");
}

/// Build tools map JSON value from project_setup (for protocol + files).
pub fn tools_from_setup(setup: Option<&ProjectSetup>) -> BTreeMap<String, WorkspaceTool> {
    setup.map(|s| s.tools.clone()).unwrap_or_default()
}

/// Whether free-form shell into workspace is allowed (lab debug only).
///
/// Production path: keep disabled; workspace agent also gates on
/// `FLEET_ALLOW_RAW_EXEC` independently.
pub fn raw_exec_allowed(env_flag: Option<&str>) -> bool {
    matches!(
        env_flag.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

/// Effective raw-exec policy from process env (`FLEET_ALLOW_RAW_EXEC`).
pub fn raw_exec_from_env() -> bool {
    raw_exec_allowed(std::env::var("FLEET_ALLOW_RAW_EXEC").ok().as_deref())
}

/// ACP permission decision (default-deny).
///
/// - `auto_approve` → allow all (lab only; dangerous)
/// - empty allowlist → deny
/// - `"*"` → allow everything
/// - unrestricted shell (`bash`, `/bin/sh`, …) is **denied** unless `"*"`
///   (path prefixes do not apply to shell; use `fleet-wsexec` only)
/// - path-like entries (contain `/` or `\`, e.g. `app/`, `tests/`) gate **file
///   mutations** (write/edit/create)
/// - other entries match as case-insensitive substrings (`fleet-wsexec`, `read`)
pub fn permission_allowed(auto_approve: bool, allow: &[String], haystack: &str) -> bool {
    if auto_approve {
        return true;
    }
    if allow.is_empty() {
        return false;
    }
    let hay = haystack.to_ascii_lowercase();
    if allow.iter().any(|a| a.trim() == "*") {
        return true;
    }

    if looks_like_unrestricted_shell(&hay) {
        return false;
    }

    let path_allows: Vec<String> = allow
        .iter()
        .map(|a| a.trim().to_ascii_lowercase())
        .filter(|a| !a.is_empty() && (a.contains('/') || a.contains('\\')))
        .collect();
    let token_allows: Vec<String> = allow
        .iter()
        .map(|a| a.trim().to_ascii_lowercase())
        .filter(|a| {
            !a.is_empty()
                && !a.contains('/')
                && !a.contains('\\')
                && !matches!(a.as_str(), "bash" | "sh" | "zsh" | "dash" | "shell")
        })
        .collect();

    let is_file_mutation = [
        "writetextfile",
        "write_text_file",
        "edit",
        "create",
        "write",
        "apply_patch",
        "str_replace",
    ]
    .iter()
    .any(|k| hay.contains(k));

    if is_file_mutation && !path_allows.is_empty() {
        return path_allows.iter().any(|p| hay.contains(p));
    }

    token_allows.iter().any(|t| hay.contains(t)) || path_allows.iter().any(|p| hay.contains(p))
}

fn looks_like_unrestricted_shell(hay: &str) -> bool {
    hay.split(|c: char| {
        !(c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '/' || c == '\\')
    })
    .any(|tok| {
        if tok.is_empty() {
            return false;
        }
        let base = tok.rsplit(|c| c == '/' || c == '\\').next().unwrap_or(tok);
        matches!(base, "bash" | "sh" | "zsh" | "dash" | "busybox" | "shell")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::WorkspaceTool;

    #[test]
    fn jobs_root_default_is_workspace_jobs() {
        assert_eq!(jobs_root_from(None), PathBuf::from("/workspace/jobs"));
        assert_eq!(jobs_root_from(Some("")), PathBuf::from("/workspace/jobs"));
        assert_eq!(
            jobs_root_from(Some("   ")),
            PathBuf::from("/workspace/jobs")
        );
    }

    #[test]
    fn jobs_root_from_explicit_path() {
        assert_eq!(
            jobs_root_from(Some("/custom/jobs")),
            PathBuf::from("/custom/jobs")
        );
    }

    #[test]
    fn resolve_work_dir_explicit_and_default() {
        assert_eq!(
            resolve_work_dir("j1", None),
            PathBuf::from("/workspace/jobs/j1/repo")
        );
        assert_eq!(
            resolve_work_dir("j1", Some("/custom/path")),
            PathBuf::from("/custom/path")
        );
    }

    #[test]
    fn validate_job_cwd_accepts_jobs_tree() {
        validate_job_cwd(Path::new("/workspace/jobs/abc/repo")).unwrap();
        validate_job_cwd(Path::new("/workspace/jobs/abc/repo/subdir")).unwrap();
    }

    #[test]
    fn validate_job_cwd_rejects_escape_and_outside() {
        // Prefix-adjacent /workspace/repo is the only lock a starts_with("/workspace")
        // mutant fails; /tmp/evil is covered by accept_agent_work_dir_rejects_escape.
        let cases: &[(&str, &str)] = &[
            ("/workspace/repo", "cwd outside job isolation"),
            ("/workspace/jobs/x/../y", "cwd must not contain '..'"),
        ];
        for (path, class) in cases {
            let err = validate_job_cwd(Path::new(path))
                .expect_err(path)
                .to_string();
            assert!(
                err.contains(class),
                "{path}: expected error class {class:?}, got {err}"
            );
        }
    }

    #[test]
    fn validate_job_cwd_rejects_nul_in_path() {
        let err = validate_job_cwd(Path::new("/workspace/jobs/abc\0/repo"))
            .expect_err("NUL in path")
            .to_string();
        assert!(
            err.contains("cwd contains NUL"),
            "NUL in path must fail as NUL, not outside/parent: {err}"
        );
    }

    #[test]
    fn validate_job_cwd_rejects_jobs_root_prefix_sibling() {
        // starts_with(root) without the trailing-slash join would accept these.
        for path in ["/workspace/jobs_evil", "/workspace/jobs2/x"] {
            let err = validate_job_cwd(Path::new(path))
                .expect_err(path)
                .to_string();
            assert!(
                err.contains("cwd outside job isolation"),
                "{path}: prefix-sibling of jobs_root must be outside, got {err}"
            );
        }
    }

    #[test]
    fn resolve_named_tool_rejects_unknown() {
        let mut tools = BTreeMap::new();
        tools.insert(
            "django_check".into(),
            WorkspaceTool {
                argv: vec!["python".into(), "manage.py".into(), "check".into()],
                description: Some("check".into()),
            },
        );
        let err = resolve_named_tool(&tools, "rm_rf").unwrap_err().to_string();
        assert!(err.contains("unknown tool"), "{err}");
        assert!(err.contains("rm_rf"), "unknown name must appear: {err}");
        assert!(err.contains("allowed="), "allowed= must appear: {err}");
        assert!(
            err.contains("django_check"),
            "allowed keys must appear: {err}"
        );
    }

    #[test]
    fn resolve_named_tool_rejects_empty_argv() {
        let mut tools = BTreeMap::new();
        tools.insert(
            "foo".into(),
            WorkspaceTool {
                argv: vec![],
                description: None,
            },
        );
        let err = resolve_named_tool(&tools, "foo").unwrap_err().to_string();
        assert!(
            err.contains("empty argv"),
            "empty argv must fail as empty argv, got {err}"
        );
    }

    #[test]
    fn resolve_named_tool_rejects_non_allowlisted_argv0() {
        for argv in [vec!["bash".into()], vec!["curl".into(), "http://x".into()]] {
            let mut tools = BTreeMap::new();
            tools.insert(
                "evil".into(),
                WorkspaceTool {
                    argv: argv.clone(),
                    description: None,
                },
            );
            let err = resolve_named_tool(&tools, "evil")
                .expect_err("non-allowlisted argv[0]")
                .to_string();
            assert!(
                err.contains("argv[0] not allowlisted"),
                "{argv:?}: expected allowlist reject via resolve_named_tool, got {err}"
            );
        }
    }

    #[test]
    fn resolve_named_tool_rejects_metachar_argv() {
        for needle in ['\n', '\r', ';', '|', '&', '$', '`'] {
            let arg = format!("a{needle}b");
            let mut tools = BTreeMap::new();
            tools.insert(
                "bad".into(),
                WorkspaceTool {
                    argv: vec!["echo".into(), arg.clone()],
                    description: None,
                },
            );
            let err = resolve_named_tool(&tools, "bad")
                .expect_err(&arg)
                .to_string();
            assert!(
                err.contains("argv rejected"),
                "{arg:?}: expected argv rejected, got {err}"
            );
        }
    }

    #[test]
    fn raw_exec_default_off_unless_fleet_allow_raw_exec() {
        // Missing / empty / explicit off — production default.
        for off in [
            None,
            Some(""),
            Some("   "),
            Some("0"),
            Some("false"),
            Some("no"),
            Some("off"),
            Some("FALSE"),
        ] {
            assert!(!raw_exec_allowed(off), "expected raw exec off for {off:?}");
        }
        // FLEET_ALLOW_RAW_EXEC required; 1/true/yes/on, trim + case-insensitive.
        for on in ["1", "true", "yes", "on", "TRUE", " Yes ", "ON", " 1 "] {
            assert!(
                raw_exec_allowed(Some(on)),
                "expected raw exec on for {on:?}"
            );
        }
    }

    #[test]
    fn permission_empty_allowlist_denies_non_shell_hay() {
        // Non-shell hay: "bash npm test" is also denied by looks_like_unrestricted_shell.
        assert!(!permission_allowed(
            false,
            &[],
            "tool fleet-wsexec django_check"
        ));
    }

    #[test]
    fn permission_star_allows_any_hay() {
        assert!(permission_allowed(false, &["*".into()], "anything"));
    }

    #[test]
    fn permission_star_entry_trims_whitespace() {
        assert!(permission_allowed(false, &[" * ".into()], "anything"));
    }

    #[test]
    fn permission_token_substring_hit() {
        assert!(permission_allowed(
            false,
            &["fleet-wsexec".into(), "read".into()],
            "tool fleet-wsexec django_check"
        ));
    }

    #[test]
    fn permission_token_substring_miss() {
        assert!(!permission_allowed(
            false,
            &["read".into()],
            "tool fleet-wsexec django_check"
        ));
    }

    #[test]
    fn permission_token_match_is_case_insensitive() {
        assert!(permission_allowed(
            false,
            &["FLEET-WSEXEC".into()],
            "tool Fleet-Wsexec django_check"
        ));
    }

    #[test]
    fn permission_auto_approve_allows_empty_allowlist() {
        assert!(permission_allowed(true, &[], "denied otherwise"));
    }

    #[test]
    fn permission_file_edit_requires_allowed_folder() {
        let allow = vec!["app/".into(), "tests/".into(), "write".into()];
        let under_app =
            "request_permission write path=/workspace/jobs/j/repo/app/fleet_marker.py";
        assert!(
            permission_allowed(false, &allow, under_app),
            "write under app/ must pass the path-gate: {under_app}"
        );
        // write token listed — still denied outside app/|tests/
        let outside = "request_permission write path=/workspace/jobs/j/repo/settings.py";
        assert!(
            !permission_allowed(false, &allow, outside),
            "write outside app/|tests/ must be denied even if token write is listed: {outside}"
        );
    }

    #[test]
    fn permission_write_token_allows_when_no_path_prefixes() {
        let hay = "write path=settings.py";
        assert!(
            permission_allowed(false, &["write".into()], hay),
            "path-gate must not run when path_allows is empty; token write grants: {hay}"
        );
    }

    #[test]
    fn permission_apply_patch_or_create_outside_folder_denied() {
        let allow = vec!["app/".into(), "create".into(), "apply_patch".into()];
        let hay = "apply_patch path=settings.py";
        assert!(
            !permission_allowed(false, &allow, hay),
            "apply_patch outside app/ must be denied as a file mutation: {hay}"
        );
    }

    #[test]
    fn bash_substring_in_filename_is_not_unrestricted_shell() {
        assert!(permission_allowed(
            false,
            &["app/".into(), "fleet-wsexec".into()],
            "write path=app/bash_utils.py"
        ));
        assert!(!permission_allowed(
            false,
            &["app/".into()],
            "bash -lc 'rm -rf /'"
        ));
    }

    #[test]
    fn bash_in_allowlist_cannot_write_outside_paths() {
        // Hay matches leftover path/token (app/, fleet-wsexec) so skip-veto-when-bash-listed would allow.
        let allow = vec![
            "app/".into(),
            "tests/".into(),
            "fleet-wsexec".into(),
            "bash".into(),
        ];
        let hay = "bash -c 'echo x > app/foo.py'";
        assert!(
            !permission_allowed(false, &allow, hay),
            "listing bash on permission_allow must not grant a shell even when hay contains app/: {hay}"
        );
    }

    #[test]
    fn bin_sh_and_busybox_denied_without_star() {
        let allow = vec!["app/".into()];
        let sh = "/bin/sh -c id";
        assert!(
            !permission_allowed(false, &allow, sh),
            "basename sh must be unrestricted shell even as /bin/sh: {sh}"
        );
        let busybox = "busybox sh -c id";
        assert!(
            !permission_allowed(false, &allow, busybox),
            "busybox must be unrestricted shell: {busybox}"
        );
    }

    #[test]
    fn validate_tool_bin_rejects_curl_and_bash() {
        // Named denies + path-prefixed curl/bash + one unknown so a
        // bash/curl/wget denylist (or allowing /usr/bin/curl) fails.
        for bin in [
            "bash",
            "sh",
            "curl",
            "wget",
            "sudo",
            "/usr/bin/curl",
            "/bin/bash",
            "rm",
        ] {
            assert!(
                validate_tool_bin(bin).is_err(),
                "{bin} must not be an allowlisted tool binary"
            );
        }
    }
}
