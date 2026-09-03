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
    fn default_work_dir_is_per_job() {
        let a = default_job_work_dir("aaa");
        let b = default_job_work_dir("bbb");
        assert_eq!(a, PathBuf::from("/workspace/jobs/aaa/repo"));
        assert_ne!(a, b);
        assert!(a.starts_with("/workspace/jobs/aaa"));
    }

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
    fn two_jobs_do_not_share_default_worktree() {
        let w1 = resolve_work_dir("job-1", None);
        let w2 = resolve_work_dir("job-2", None);
        assert_ne!(w1, w2);
        assert!(w1.to_string_lossy().contains("job-1"));
        assert!(w2.to_string_lossy().contains("job-2"));
    }

    #[test]
    fn validate_job_cwd_accepts_jobs_tree() {
        validate_job_cwd(Path::new("/workspace/jobs/abc/repo")).unwrap();
        validate_job_cwd(Path::new("/workspace/jobs/abc/repo/subdir")).unwrap();
    }

    #[test]
    fn validate_job_cwd_rejects_escape_and_outside() {
        assert!(validate_job_cwd(Path::new("/workspace/repo")).is_err());
        assert!(validate_job_cwd(Path::new("/tmp/evil")).is_err());
        assert!(validate_job_cwd(Path::new("/workspace/jobs/x/../y")).is_err());
    }

    #[test]
    fn resolve_named_tool_ok_and_unknown() {
        let mut tools = BTreeMap::new();
        tools.insert(
            "django_check".into(),
            WorkspaceTool {
                argv: vec!["python".into(), "manage.py".into(), "check".into()],
                description: Some("check".into()),
            },
        );
        let t = resolve_named_tool(&tools, "django_check").unwrap();
        assert_eq!(t.argv[0], "python");
        let err = resolve_named_tool(&tools, "rm_rf").unwrap_err().to_string();
        assert!(err.contains("unknown tool"), "{err}");
    }

    #[test]
    fn resolve_named_tool_rejects_metachar_argv() {
        let mut tools = BTreeMap::new();
        tools.insert(
            "evil".into(),
            WorkspaceTool {
                argv: vec![
                    "python".into(),
                    "-c".into(),
                    "import os; os.system('x')".into(),
                ],
                description: None,
            },
        );
        // semicolon not in that string - add one with ;
        tools.insert(
            "bad".into(),
            WorkspaceTool {
                argv: vec!["echo".into(), "a;b".into()],
                description: None,
            },
        );
        assert!(resolve_named_tool(&tools, "bad").is_err());
    }

    #[test]
    fn raw_exec_flag() {
        assert!(!raw_exec_allowed(None));
        assert!(!raw_exec_allowed(Some("0")));
        assert!(raw_exec_allowed(Some("1")));
        assert!(raw_exec_allowed(Some("true")));
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
    fn permission_default_deny_and_star() {
        assert!(!permission_allowed(false, &[], "bash npm test"));
        assert!(permission_allowed(false, &["*".into()], "anything"));
        assert!(permission_allowed(
            false,
            &["fleet-wsexec".into(), "read".into()],
            "tool fleet-wsexec django_check"
        ));
        assert!(!permission_allowed(
            false,
            &["read".into()],
            "tool fleet-wsexec django_check"
        ));
        assert!(permission_allowed(true, &[], "denied otherwise"));
    }

    #[test]
    fn permission_file_edit_requires_allowed_folder() {
        let allow = vec![
            "app/".into(),
            "tests/".into(),
            "fleet-wsexec".into(),
            "read".into(),
            "write".into(),
        ];
        // write under allowed project folders
        assert!(permission_allowed(
            false,
            &allow,
            "request_permission write path=/workspace/jobs/j/repo/app/fleet_marker.py"
        ));
        assert!(permission_allowed(
            false,
            &allow,
            "edit tests/test_fleet_smoke.py"
        ));
        // write outside allowed folders — denied even if "write" is listed
        assert!(!permission_allowed(
            false,
            &allow,
            "request_permission write path=/workspace/jobs/j/repo/settings.py"
        ));
        assert!(permission_allowed(
            false,
            &allow,
            "fleet-wsexec django_check"
        ));
        assert!(permission_allowed(false, &allow, "read FLEET_TOOLS.md"));
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
        let allow = vec![
            "app/".into(),
            "tests/".into(),
            "fleet-wsexec".into(),
            "bash".into(),
        ];
        assert!(!permission_allowed(
            false,
            &allow,
            "request_permission bash -c 'echo x > settings.py'"
        ));
        assert!(!permission_allowed(
            false,
            &allow,
            "tool bash run invoices/models/animal.py"
        ));
    }

    #[test]
    fn validate_tool_bin_rejects_curl_and_bash() {
        assert!(validate_tool_bin("python").is_ok());
        assert!(validate_tool_bin("/usr/bin/python3").is_ok());
        assert!(validate_tool_bin("bash").is_err());
        assert!(validate_tool_bin("curl").is_err());
        assert!(validate_tool_bin("wget").is_err());
    }

    #[test]
    fn django_and_node_setup_shapes() {
        let django: ProjectSetup = serde_json::from_value(serde_json::json!({
            "strategy": "commands",
            "commands": ["python -m pip install -r requirements.txt"],
            "smoke_commands": ["python manage.py check"],
            "tools": {
                "django_check": { "argv": ["python", "manage.py", "check"] }
            }
        }))
        .unwrap();
        assert_eq!(django.commands.len(), 1);
        assert!(django.tools.contains_key("django_check"));

        let node: ProjectSetup = serde_json::from_value(serde_json::json!({
            "strategy": "commands",
            "commands": ["npm ci"],
            "tools": {
                "npm_test": { "argv": ["npm", "test"], "description": "unit tests" }
            }
        }))
        .unwrap();
        assert_eq!(node.commands, vec!["npm ci"]);
        let t = resolve_named_tool(&node.tools, "npm_test").unwrap();
        assert_eq!(t.argv, vec!["npm", "test"]);
    }
}
