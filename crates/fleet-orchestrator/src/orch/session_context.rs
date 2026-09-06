//! Session work-dir files + env for workers / fleet-driver (PR2).

use crate::store::BoardTaskRow;
use serde_json::{Value, json};
use std::path::Path;

/// JSON array of strings → `Vec<String>`; anything else → empty + warn.
pub fn acceptance_from_json(v: &Value) -> Vec<String> {
    match v {
        Value::Array(items) => items
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect(),
        other => {
            if !other.is_null() {
                tracing::warn!(
                    ?other,
                    "board_tasks.acceptance not a JSON array of strings; using empty"
                );
            }
            vec![]
        }
    }
}

pub fn compose_prompt(title: &str, summary: &str, spec_md: &str, acceptance: &[String]) -> String {
    let mut out = String::new();
    if !title.trim().is_empty() {
        out.push_str("# ");
        out.push_str(title.trim());
        out.push_str("\n\n");
    }
    if !summary.trim().is_empty() {
        out.push_str(summary.trim());
        out.push_str("\n\n");
    }
    if !spec_md.trim().is_empty() {
        out.push_str(spec_md.trim());
        out.push_str("\n\n");
    }
    if !acceptance.is_empty() {
        out.push_str("## Acceptance\n\n");
        for line in acceptance {
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            out.push_str("- ");
            out.push_str(t);
            out.push('\n');
        }
        out.push('\n');
    }
    if out.trim().is_empty() {
        return "Inspect this workspace and complete the assigned task.".into();
    }
    out
}

/// Whole-run context so every sandbox sees the board, not just its own spec.
pub fn write_run_context(
    work_dir: &Path,
    you_plan_id: &str,
    tasks: &[&BoardTaskRow],
    shared_files: &[String],
) -> std::io::Result<()> {
    let mut out = String::from("# Run context\n\n");
    out.push_str(
        "This is one fleet run. You are **one** specialist. \
         The run is not finished until every task on the board is done. \
         Do not stop in the middle. Later agents will read `work/`, `result.md`, and this file.\n\n",
    );
    out.push_str(&format!("You are `{you_plan_id}`.\n\n"));
    out.push_str("## Goal\n\n");
    if let Some(first) = tasks.first() {
        let goal = if !first.summary.trim().is_empty() {
            first.summary.trim()
        } else {
            first.title.trim()
        };
        out.push_str(goal);
        out.push_str("\n\n");
    }
    out.push_str("## Board\n\n");
    out.push_str(
        "| id | title | status | depends | acceptance |\n| --- | --- | --- | --- | --- |\n",
    );
    let id_to_plan: std::collections::HashMap<uuid::Uuid, String> = tasks
        .iter()
        .map(|t| (t.id, t.plan_task_id.clone()))
        .collect();
    for t in tasks {
        let deps = t
            .depends_on
            .iter()
            .filter_map(|id| id_to_plan.get(id).cloned())
            .collect::<Vec<_>>()
            .join(",");
        let acc = acceptance_from_json(&t.acceptance).join("; ");
        let mark = if t.plan_task_id == you_plan_id {
            " ← you"
        } else {
            ""
        };
        out.push_str(&format!(
            "| `{}` | {} | {}{} | {} | {} |\n",
            t.plan_task_id,
            t.title.replace('|', "/"),
            t.status,
            mark,
            deps,
            acc.replace('|', "/"),
        ));
    }
    out.push('\n');
    if let Some(you) = tasks.iter().find(|t| t.plan_task_id == you_plan_id) {
        out.push_str("## Your spec\n\n");
        out.push_str(you.spec_md.trim());
        out.push_str("\n\n## Your acceptance (must be true before this task is done)\n\n");
        let acc = acceptance_from_json(&you.acceptance);
        if acc.is_empty() {
            out.push_str("- result.md exists and is non-empty\n");
        } else {
            for line in acc {
                out.push_str("- ");
                out.push_str(&line);
                out.push('\n');
            }
        }
        out.push('\n');
    }
    out.push_str("## Shared files in `work/`\n\n");
    if shared_files.is_empty() {
        out.push_str("(empty — you may be first)\n");
    } else {
        for f in shared_files.iter().take(40) {
            out.push_str("- `");
            out.push_str(f);
            out.push_str("`\n");
        }
    }
    out.push_str(
        "\nWhen finished: write `result.md` (what you did, paths, what the next agent should do). \
         Put artifacts in `work/`. Do not exit until acceptance is met.\n",
    );
    std::fs::write(work_dir.join("RUN.md"), out)
}

pub fn list_shared_files(work_root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if crate::sandbox::share::skip_name(&name) {
                continue;
            }
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if e.path().is_dir() {
                if out.len() >= 40 {
                    return;
                }
                walk(&e.path(), &rel, out);
            } else {
                out.push(rel);
                if out.len() >= 40 {
                    return;
                }
            }
        }
    }
    walk(work_root, "", &mut out);
    out.sort();
    out
}

/// Write `spec.md`, `task.json`, `prompt.md`, and optional `workspace.json`.
pub fn write_session_context(
    work_dir: &Path,
    task: &BoardTaskRow,
    timeout_s: u64,
    repo: Option<&crate::domain::ProjectGitRemote>,
) -> std::io::Result<()> {
    if !task.spec_md.is_empty() {
        std::fs::write(work_dir.join("spec.md"), &task.spec_md)?;
    }
    let acceptance = acceptance_from_json(&task.acceptance);
    let mut task_json = json!({
        "title": task.title,
        "summary": task.summary,
        "spec_md": task.spec_md,
        "acceptance": acceptance,
        "plan_task_id": task.plan_task_id,
        "worker_profile": task.worker_profile,
        "timeout_s": timeout_s,
    });
    if let Some(repo) = repo {
        task_json["repo_url"] = json!(repo.url);
        task_json["git_ref"] = json!(repo.git_ref);
        let workspace = json!({
            "repo": {
                "url": repo.url,
                "ref": repo.git_ref,
                "auth_via": "proxy"
            }
        });
        std::fs::write(
            work_dir.join("workspace.json"),
            serde_json::to_vec_pretty(&workspace).unwrap_or_else(|_| b"{}".to_vec()),
        )?;
    }
    std::fs::write(
        work_dir.join("task.json"),
        serde_json::to_vec_pretty(&task_json).unwrap_or_else(|_| b"{}".to_vec()),
    )?;
    let prompt = compose_prompt(&task.title, &task.summary, &task.spec_md, &acceptance);
    std::fs::write(work_dir.join("prompt.md"), prompt)?;
    Ok(())
}

/// Copy selected process env into the worker env map (so k8s/process both see them).
pub fn pass_through_driver_env(env: &mut std::collections::HashMap<String, String>) {
    for key in [
        "FLEET_MODEL",
        "FLEET_AUTO_APPROVE",
        "FLEET_HARNESS_CMD",
        "FLEET_MOCK_ACP_BIN",
        "FLEET_GROK_BIN",
        "FLEET_REASONING_EFFORT",
        "FLEET_GROK_DEBUG",
    ] {
        if let Ok(v) = std::env::var(key)
            && !v.trim().is_empty()
        {
            env.insert(key.into(), v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Mutex, OnceLock};
    use tempfile::tempdir;
    use uuid::Uuid;

    fn sample_task(acceptance: Value) -> BoardTaskRow {
        BoardTaskRow {
            id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            plan_id: Uuid::new_v4(),
            plan_task_id: "t1".into(),
            title: "Add marker".into(),
            summary: "Tiny change".into(),
            spec_md: "Write hello.txt.".into(),
            status: "ready".into(),
            depends_on: vec![],
            priority: 0,
            worker_profile: "coding".into(),
            acceptance,
            external_ref: None,
            timeout_s: Some(120),
            max_retries: 2,
            attempt: 0,
            claimed_session_id: None,
            last_report_seq: 0,
            progress_pct: 0,
            block_reason: None,
            hitl_notes: None,
            hitl_artifacts: None,
            hitl_resume_requested_at: None,
            hitl_notified_at: None,
            hitl_resolved_at: None,
            failure_reason: None,
            artifacts: None,
            retry_not_before: None,
            assigned_at: None,
            started_at: None,
            terminal_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn acceptance_array_of_strings() {
        let v = json!(["file exists", "contains hello"]);
        assert_eq!(
            acceptance_from_json(&v),
            vec!["file exists", "contains hello"]
        );
    }

    #[test]
    fn acceptance_non_array_empty() {
        assert!(acceptance_from_json(&json!({"nope": true})).is_empty());
        assert!(acceptance_from_json(&Value::Null).is_empty());
    }

    #[test]
    fn compose_includes_sections() {
        let p = compose_prompt(
            "Add marker",
            "Tiny change",
            "Write hello.txt.",
            &["file exists".into()],
        );
        assert!(p.contains("# Add marker"));
        assert!(p.contains("Tiny change"));
        assert!(p.contains("Write hello.txt."));
        assert!(p.contains("- file exists"));
    }

    /// Serializes env mutation in this module so parallel tests do not race.
    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn with_env<T>(pairs: &[(&str, Option<&str>)], f: impl FnOnce() -> T) -> T {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let previous: Vec<(&str, Option<String>)> = pairs
            .iter()
            .map(|(k, _)| (*k, std::env::var(k).ok()))
            .collect();
        // SAFETY: held under env_lock; restored before unlock / panic resume.
        unsafe {
            for (k, v) in pairs {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        // SAFETY: restore prior process env before releasing the lock.
        unsafe {
            for (k, v) in previous {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
        match result {
            Ok(v) => v,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    #[test]
    fn pass_through_does_not_copy_fleet_harness() {
        with_env(&[("FLEET_HARNESS", Some("mock"))], || {
            let mut env = std::collections::HashMap::new();
            pass_through_driver_env(&mut env);
            assert!(
                !env.contains_key("FLEET_HARNESS"),
                "plane FLEET_HARNESS must not override spawn LLM choice: {env:?}"
            );
        });
    }

    #[test]
    fn pass_through_copies_nonempty_allowlisted_keys() {
        with_env(
            &[
                ("FLEET_MODEL", Some("grok-4")),
                ("FLEET_AUTO_APPROVE", Some("1")),
                ("FLEET_HARNESS_CMD", Some("/opt/mock-acp-agent")),
                ("FLEET_MOCK_ACP_BIN", Some("/opt/mock-acp-agent")),
                ("FLEET_GROK_BIN", Some("/opt/grok")),
                ("FLEET_REASONING_EFFORT", Some("high")),
                ("FLEET_GROK_DEBUG", Some("1")),
            ],
            || {
                let mut env = std::collections::HashMap::new();
                pass_through_driver_env(&mut env);
                assert_eq!(
                    env,
                    std::collections::HashMap::from([
                        ("FLEET_MODEL".into(), "grok-4".into()),
                        ("FLEET_AUTO_APPROVE".into(), "1".into()),
                        ("FLEET_HARNESS_CMD".into(), "/opt/mock-acp-agent".into()),
                        ("FLEET_MOCK_ACP_BIN".into(), "/opt/mock-acp-agent".into()),
                        ("FLEET_GROK_BIN".into(), "/opt/grok".into()),
                        ("FLEET_REASONING_EFFORT".into(), "high".into()),
                        ("FLEET_GROK_DEBUG".into(), "1".into()),
                    ])
                );
            },
        );
    }

    #[test]
    fn pass_through_skips_blank_or_whitespace_allowlisted_values() {
        let keys = [
            "FLEET_MODEL",
            "FLEET_AUTO_APPROVE",
            "FLEET_HARNESS_CMD",
            "FLEET_MOCK_ACP_BIN",
            "FLEET_GROK_BIN",
            "FLEET_REASONING_EFFORT",
            "FLEET_GROK_DEBUG",
        ];
        let empty: Vec<(&str, Option<&str>)> = keys.iter().map(|k| (*k, Some(""))).collect();
        with_env(&empty, || {
            let mut env = std::collections::HashMap::new();
            pass_through_driver_env(&mut env);
            for key in keys {
                assert!(
                    !env.contains_key(key),
                    "blank {key} must not be copied: {env:?}"
                );
            }
        });
        let whitespace: Vec<(&str, Option<&str>)> =
            keys.iter().map(|k| (*k, Some(" \t "))).collect();
        with_env(&whitespace, || {
            let mut env = std::collections::HashMap::new();
            pass_through_driver_env(&mut env);
            for key in keys {
                assert!(
                    !env.contains_key(key),
                    "whitespace {key} must not be copied: {env:?}"
                );
            }
        });
    }

    #[test]
    fn pass_through_does_not_copy_unlisted_fleet_keys() {
        with_env(
            &[
                ("FLEET_WORKER_TOKEN", Some("tok")),
                ("FLEET_ORCHESTRATOR_URL", Some("http://orch.test")),
                ("FLEET_REPO_URL", Some("https://git.test/repo.git")),
                ("FLEET_WORK_CMD", Some("echo hi")),
            ],
            || {
                let mut env = std::collections::HashMap::new();
                pass_through_driver_env(&mut env);
                for key in [
                    "FLEET_WORKER_TOKEN",
                    "FLEET_ORCHESTRATOR_URL",
                    "FLEET_REPO_URL",
                    "FLEET_WORK_CMD",
                ] {
                    assert!(
                        !env.contains_key(key),
                        "unlisted {key} must not be copied: {env:?}"
                    );
                }
            },
        );
    }

    #[test]
    fn write_files_roundtrip() {
        let dir = tempdir().unwrap();
        let task = sample_task(json!(["ok"]));
        write_session_context(dir.path(), &task, 90, None).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("spec.md")).unwrap(),
            "Write hello.txt."
        );
        let tj: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("task.json")).unwrap())
                .unwrap();
        assert_eq!(tj["title"], "Add marker");
        assert_eq!(tj["timeout_s"], 90);
        assert_eq!(tj["acceptance"], json!(["ok"]));
        let prompt = std::fs::read_to_string(dir.path().join("prompt.md")).unwrap();
        assert!(prompt.contains("# Add marker"));
        assert!(prompt.contains("- ok"));
        assert!(!dir.path().join("workspace.json").exists());
    }

    #[test]
    fn run_context_names_every_task() {
        let dir = tempdir().unwrap();
        let t1 = sample_task(json!(["ok"]));
        let mut t2 = sample_task(json!(["ok"]));
        t2.plan_task_id = "t2".into();
        write_run_context(dir.path(), "t1", &[&t1, &t2], &[]).unwrap();
        let md = std::fs::read_to_string(dir.path().join("RUN.md")).unwrap();
        let board = md
            .split_once("## Board")
            .map(|(_, rest)| rest.split("\n## ").next().unwrap_or(rest))
            .unwrap_or("");
        assert!(
            board.contains("| `t1` |"),
            "board table must name t1 (You are `t1` is not enough): {md}"
        );
        assert!(
            board.contains("| `t2` |"),
            "board table must name t2: {md}"
        );
        assert_eq!(
            md.matches("← you").count(),
            1,
            "exactly one ← you mark: {md}"
        );
        let you_row = board.lines().find(|l| l.contains("| `t1` |")).unwrap_or("");
        assert!(
            you_row.contains("← you"),
            "← you must sit on the t1 board row: {md}"
        );
    }

    #[test]
    fn run_context_empty_acceptance_defaults_result_md() {
        let dir = tempdir().unwrap();
        let t = sample_task(json!([]));
        write_run_context(dir.path(), "t1", &[&t], &[]).unwrap();
        let md = std::fs::read_to_string(dir.path().join("RUN.md")).unwrap();
        let yours = md
            .split_once("## Your acceptance")
            .map(|(_, rest)| rest)
            .unwrap_or("");
        assert!(
            yours.contains("- result.md exists and is non-empty"),
            "empty you-task acceptance must list result.md exists and is non-empty: {md}"
        );
    }

    #[test]
    fn run_context_depends_on_renders_plan_task_ids() {
        let dir = tempdir().unwrap();
        let t1 = sample_task(json!(["ok"]));
        let mut t2 = sample_task(json!(["ok"]));
        t2.plan_task_id = "t2".into();
        t2.depends_on = vec![t1.id];
        write_run_context(dir.path(), "t1", &[&t1, &t2], &[]).unwrap();
        let md = std::fs::read_to_string(dir.path().join("RUN.md")).unwrap();
        let t2_row = md.lines().find(|l| l.contains("| `t2` |")).unwrap_or("");
        let cells: Vec<&str> = t2_row
            .split('|')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(
            cells.get(3).copied().unwrap_or(""),
            "t1",
            "t2 depends cell must be plan id t1, not UUID {}: {md}",
            t1.id
        );
        assert!(
            !md.contains(&t1.id.to_string()),
            "board must not leak task UUID {}: {md}",
            t1.id
        );
    }

    #[test]
    fn list_shared_files_skips_meta_and_caps_at_40() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("TEAM.md"), "meta").unwrap();
        for i in 0..41 {
            std::fs::write(dir.path().join(format!("f{i:02}.txt")), "x").unwrap();
        }
        let files = list_shared_files(dir.path());
        assert_eq!(
            files.len(),
            40,
            "list_shared_files must cap at 40: {files:?}"
        );
        assert!(
            files.iter().all(|f| f != "TEAM.md"),
            "TEAM.md must be omitted: {files:?}"
        );
        let mut sorted = files.clone();
        sorted.sort();
        assert_eq!(files, sorted, "shared files must be sorted: {files:?}");
    }
}
