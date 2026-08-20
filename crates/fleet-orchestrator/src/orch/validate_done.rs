//! Gate: a task is not `done_success` until the workspace proves it.
//!
//! Stub sandboxes have no files — they skip the file checks so existing
//! fixture tests still pass. Process / local / docker must produce `result.md`
//! and meet `exists:` / `contains:` acceptance lines.

use std::path::Path;

use crate::store::BoardTaskRow;

use super::session_context::acceptance_from_json;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoneCheck {
    pub ok: bool,
    pub reasons: Vec<String>,
}

impl DoneCheck {
    pub fn pass() -> Self {
        Self {
            ok: true,
            reasons: vec!["ok".into()],
        }
    }

    pub fn fail(reasons: Vec<String>) -> Self {
        Self { ok: false, reasons }
    }

    pub fn notes(&self) -> String {
        let mut out = String::from("## Task validation failed — finish these before the run can continue\n\n");
        for r in &self.reasons {
            out.push_str("- ");
            out.push_str(r);
            out.push('\n');
        }
        out.push_str(
            "\nThe fleet will send you back. Read RUN.md and TEAM.md. \
             Write `result.md` and put artifacts in `work/`.\n",
        );
        out
    }
}

/// Default acceptance when the planner left the list empty.
pub fn default_acceptance(worker_profile: &str) -> Vec<String> {
    let p = worker_profile.trim().to_ascii_lowercase();
    if p.contains("review") || p.contains("test") {
        return vec![
            "result.md exists and is non-empty".into(),
            "result.md states pass or fail".into(),
        ];
    }
    vec![
        "result.md exists and is non-empty".into(),
        "exists:work".into(),
    ]
}

pub fn validate_task_done(
    task: &BoardTaskRow,
    work_dir: &Path,
    runtime_backend: &str,
) -> DoneCheck {
    let backend = runtime_backend.trim().to_ascii_lowercase();
    if backend == "stub" || backend.is_empty() {
        return DoneCheck::pass();
    }
    if !work_dir.exists() {
        return DoneCheck::fail(vec!["session work dir missing".into()]);
    }

    let mut reasons = Vec::new();
    let result = work_dir.join("result.md");
    let result_body = std::fs::read_to_string(&result).unwrap_or_default();
    let result_trim = result_body.trim();
    let github_elsewhere = workspace_cites_github(work_dir);
    if result_trim.is_empty() {
        if !github_elsewhere {
            reasons.push("result.md is missing or empty — next agents have no context".into());
        }
    } else if result_trim.chars().count() < 20 && !github_elsewhere {
        reasons.push("result.md is too short to hand off".into());
    }

    if looks_blocked(result_trim) && !looks_complete(result_trim) && !github_elsewhere {
        reasons.push("result.md reports a blocker without completing the task".into());
    }

    if task_must_cite_github(task) && !cites_github(result_trim) && !github_elsewhere {
        reasons.push(
            "this task must push to GitHub — result.md needs a github.com PR or commit URL"
                .into(),
        );
    }

    if let Ok(raw) = std::fs::read_to_string(work_dir.join(".fleet").join("done.json")) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            if v.get("ok").and_then(|x| x.as_bool()) == Some(false) {
                let why = v
                    .get("reason")
                    .and_then(|x| x.as_str())
                    .unwrap_or("done.json ok=false");
                reasons.push(format!("agent marked not done: {why}"));
            }
        }
    }

    let mut acceptance = acceptance_from_json(&task.acceptance);
    if acceptance.is_empty() {
        acceptance = default_acceptance(&task.worker_profile);
    }
    for line in &acceptance {
        if let Some(fail) = check_acceptance_line(work_dir, line, result_trim) {
            reasons.push(fail);
        }
    }

    if reasons.is_empty() {
        DoneCheck::pass()
    } else {
        DoneCheck::fail(reasons)
    }
}

pub fn task_must_cite_github(task: &BoardTaskRow) -> bool {
    let id = task.plan_task_id.to_ascii_lowercase();
    let profile = task.worker_profile.to_ascii_lowercase();
    if id == "ship" || profile.contains("ship") || profile.contains("release") {
        return true;
    }
    let spec = task.spec_md.to_ascii_lowercase();
    spec.contains("must")
        && (spec.contains("create_pr")
            || spec.contains("commit_files")
            || spec.contains("pr url")
            || spec.contains("github.com"))
}

fn cites_github(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("github.com")
        || t.contains("gitlab.com")
        || t.contains("/pull/")
        || t.contains("/merge_requests/")
}

/// Ship often writes the PR URL into work/SHIP.md before result.md.
fn workspace_cites_github(work_dir: &Path) -> bool {
    for rel in [
        "SHIP.md",
        "work/SHIP.md",
        "work/review_notes.md",
        "handoff/result.md",
    ] {
        let body = std::fs::read_to_string(work_dir.join(rel)).unwrap_or_default();
        if cites_github(&body) {
            return true;
        }
    }
    false
}

fn looks_blocked(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("blocked")
        || t.contains("cannot complete")
        || t.contains("could not")
        || t.contains("i don't know")
}

fn looks_complete(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("done") || t.contains("completed") || t.contains("shipped") || t.contains("pass")
}

fn check_acceptance_line(work_dir: &Path, line: &str, result_md: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let lower = line.to_ascii_lowercase();

    if lower == "result.md exists and is non-empty" {
        return None; // already checked
    }
    if lower == "result.md states pass or fail" {
        let t = result_md.to_ascii_lowercase();
        if t.contains("pass") || t.contains("fail") || t.contains("lgtm") || t.contains("reject") {
            return None;
        }
        return Some("result.md must say pass or fail".into());
    }

    if let Some(rest) = lower
        .strip_prefix("exists:")
        .or_else(|| lower.strip_prefix("file:"))
    {
        let rel = rest.trim().trim_start_matches('/');
        if resolve_work_path(work_dir, rel).is_some() {
            return None;
        }
        return Some(format!("missing required path `{rel}`"));
    }

    if let Some(rest) = line
        .strip_prefix("contains:")
        .or_else(|| lower.strip_prefix("contains:"))
    {
        let rest = rest.trim();
        let (path, needle) = rest.split_once(':')?;
        let path = path.trim().trim_start_matches('/');
        let needle = needle.trim();
        if needle.is_empty() {
            return None;
        }
        let Some(p) = resolve_work_path(work_dir, path) else {
            return Some(format!("contains: `{path}` does not exist"));
        };
        let body = std::fs::read_to_string(p).unwrap_or_default();
        if body.contains(needle) {
            return None;
        }
        return Some(format!("`{path}` does not contain `{needle}`"));
    }

    // Free-form: must be mentioned in result.md so the next agent can see it.
    if !result_md.is_empty() {
        let hay = result_md.to_ascii_lowercase();
        let key = line
            .split_whitespace()
            .take(3)
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase();
        if key.len() >= 4 && hay.contains(&key) {
            return None;
        }
    }
    None
}

fn resolve_work_path(work_dir: &Path, rel: &str) -> Option<std::path::PathBuf> {
    let rel = rel.trim().trim_start_matches(['/', '\\']);
    if rel.split(['/', '\\']).any(|p| p == "..") {
        return None;
    }
    let candidates = [
        work_dir.join(rel),
        work_dir.join("work").join(rel),
        work_dir.join("work"),
    ];
    for p in candidates {
        if rel == "work" && work_dir.join("work").is_dir() {
            return Some(work_dir.join("work"));
        }
        if p.exists() {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;
    use uuid::Uuid;

    fn task(acceptance: serde_json::Value) -> BoardTaskRow {
        BoardTaskRow {
            id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            plan_id: Uuid::new_v4(),
            plan_task_id: "t1".into(),
            title: "Write marker".into(),
            summary: "add hello".into(),
            spec_md: "Write work/hello.txt".into(),
            status: "in_progress".into(),
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
    fn stub_always_passes() {
        let dir = tempdir().unwrap();
        let c = validate_task_done(&task(json!([])), dir.path(), "stub");
        assert!(c.ok, "{c:?}");
    }

    #[test]
    fn process_requires_result_md() {
        let dir = tempdir().unwrap();
        let c = validate_task_done(&task(json!([])), dir.path(), "process");
        assert!(!c.ok);
        assert!(c.reasons.iter().any(|r| r.contains("result.md")), "{c:?}");
    }

    #[test]
    fn process_passes_with_result_and_work() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("result.md"), "Done. Wrote hello in work/.\n").unwrap();
        std::fs::create_dir_all(dir.path().join("work")).unwrap();
        std::fs::write(dir.path().join("work").join("hello.txt"), "hi\n").unwrap();
        let c = validate_task_done(&task(json!([])), dir.path(), "process");
        assert!(c.ok, "{c:?}");
    }

    #[test]
    fn exists_rejects_parent_escape() {
        assert!(resolve_work_path(std::path::Path::new("/tmp"), "../etc/passwd").is_none());
    }

    #[test]
    fn exists_and_contains_checks() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("result.md"), "Completed the marker file.\n").unwrap();
        std::fs::create_dir_all(dir.path().join("work")).unwrap();
        std::fs::write(dir.path().join("work").join("hello.txt"), "hello world\n").unwrap();
        let t = task(json!(["exists:hello.txt", "contains:hello.txt:hello"]));
        let c = validate_task_done(&t, dir.path(), "local");
        assert!(c.ok, "{c:?}");
    }

    #[test]
    fn missing_contains_fails() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("result.md"), "Completed the marker file.\n").unwrap();
        std::fs::create_dir_all(dir.path().join("work")).unwrap();
        std::fs::write(dir.path().join("work").join("hello.txt"), "nope\n").unwrap();
        let t = task(json!(["contains:hello.txt:hello"]));
        let c = validate_task_done(&t, dir.path(), "docker");
        assert!(!c.ok);
    }

    #[test]
    fn ship_task_requires_github_url() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("result.md"), "Pushed the calculator locally.\n").unwrap();
        std::fs::create_dir_all(dir.path().join("work")).unwrap();
        let mut t = task(json!([]));
        t.plan_task_id = "ship".into();
        let c = validate_task_done(&t, dir.path(), "process");
        assert!(!c.ok, "{c:?}");
        std::fs::write(
            dir.path().join("result.md"),
            "Opened https://github.com/acme/app/pull/9\n",
        )
        .unwrap();
        let c = validate_task_done(&t, dir.path(), "process");
        assert!(c.ok, "{c:?}");
    }

    #[test]
    fn ship_accepts_pr_url_in_work_ship_md() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("work")).unwrap();
        std::fs::write(
            dir.path().join("work").join("SHIP.md"),
            "PR: https://github.com/OmChillure/test/pull/2\n",
        )
        .unwrap();
        let mut t = task(json!([]));
        t.plan_task_id = "ship".into();
        let c = validate_task_done(&t, dir.path(), "process");
        assert!(c.ok, "{c:?}");
    }
}
