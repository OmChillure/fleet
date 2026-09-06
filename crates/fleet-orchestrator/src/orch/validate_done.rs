//! Gate: a task is not `done_success` until the workspace proves it.
//!
//! Remote guests must leave `result.md` (and acceptance) on the session work
//! dir the plane can read — via status `result_md` / `files` proof. HTTP ok
//! / lease complete is not proof.

use std::path::Path;

use uuid::Uuid;

use crate::store::BoardTaskRow;

use super::session_context::acceptance_from_json;

/// Worker-driven terminal apply (`done_success` / `done_failed` / `blocked`)
/// is allowed only from `assigned`/`in_progress` while `claimed_session_id`
/// matches the reporter. Blocked HITL stays blocked until resume. A stale
/// success after bounce (claim cleared or a new session) must not complete.
pub fn can_apply_task_terminal(
    task: &BoardTaskRow,
    reporting_session_id: Uuid,
    terminal_status: &str,
) -> bool {
    if !matches!(terminal_status, "done_success" | "done_failed" | "blocked") {
        return false;
    }
    if !matches!(task.status.as_str(), "assigned" | "in_progress") {
        return false;
    }
    task.claimed_session_id == Some(reporting_session_id)
}

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
        let mut out = String::from(
            "## Task validation failed — finish these before the run can continue\n\n",
        );
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
    if p.contains("cod") || p.contains("implement") {
        return vec![
            "result.md exists and is non-empty".into(),
            "exists:work".into(),
        ];
    }
    vec!["result.md exists and is non-empty".into()]
}

pub fn validate_task_done(
    task: &BoardTaskRow,
    work_dir: &Path,
    runtime_backend: &str,
) -> DoneCheck {
    let backend = runtime_backend.trim().to_ascii_lowercase();
    if backend == "stub" {
        return DoneCheck::pass();
    }
    if !work_dir.exists() {
        return DoneCheck::fail(vec!["session work dir missing".into()]);
    }

    let mut reasons = Vec::new();
    let result = work_dir.join("result.md");
    let result_body = std::fs::read_to_string(&result).unwrap_or_default();
    let result_trim = result_body.trim();
    if result_trim.is_empty() {
        reasons.push("result.md is missing or empty — next agents have no context".into());
    } else if result_trim.chars().count() < 20 {
        reasons.push("result.md is too short to hand off".into());
    }

    if looks_blocked(result_trim) && !looks_complete(result_trim) {
        reasons.push("result.md reports a blocker without completing the task".into());
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
    // `work` itself is the session work directory, not a fallback for other paths.
    if rel == "work" && work_dir.join("work").is_dir() {
        return Some(work_dir.join("work"));
    }
    let candidates = [work_dir.join(rel), work_dir.join("work").join(rel)];
    for p in candidates {
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
    fn k8s_and_remote_still_require_proof_of_work() {
        // Product gate: a task is not done_success until the workspace proves it.
        // HTTP ok / CR phase alone is not proof. Missing result.md must fail
        // even when the guest ran off-box (remote / k8s).
        let dir = tempdir().unwrap();
        for backend in ["k8s", "kubernetes", "remote"] {
            let missing_dir = validate_task_done(
                &task(json!([])),
                &dir.path().join("no-such-session"),
                backend,
            );
            assert!(
                !missing_dir.ok,
                "{backend} must not pass when the session dir is gone: {missing_dir:?}"
            );
            let empty = validate_task_done(&task(json!([])), dir.path(), backend);
            assert!(
                !empty.ok,
                "{backend} must not pass without result.md: {empty:?}"
            );
        }
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
        std::fs::write(
            dir.path().join("result.md"),
            "Done. Wrote hello in work/.\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("work")).unwrap();
        std::fs::write(dir.path().join("work").join("hello.txt"), "hi\n").unwrap();
        let c = validate_task_done(&task(json!([])), dir.path(), "process");
        assert!(c.ok, "{c:?}");
    }

    #[test]
    fn exists_rejects_parent_escape() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("result.md"), "Completed the marker file.\n").unwrap();
        std::fs::create_dir_all(dir.path().join("work")).unwrap();
        let t = task(json!(["exists:../etc/passwd"]));
        let c = validate_task_done(&t, dir.path(), "local");
        assert!(
            !c.ok,
            "exists:../etc/passwd must not treat a parent path as session proof: {c:?}"
        );
        assert!(
            c.reasons.iter().any(|r| r.contains("../etc/passwd")),
            "fail reason must name the escaped rel: {c:?}"
        );
    }

    #[test]
    fn contains_parent_escape_does_not_read_host_file() {
        let parent = tempdir().unwrap();
        let work_dir = parent.path().join("session");
        std::fs::create_dir_all(work_dir.join("work")).unwrap();
        std::fs::write(work_dir.join("result.md"), "Completed the marker file.\n").unwrap();
        std::fs::create_dir_all(parent.path().join("etc")).unwrap();
        std::fs::write(parent.path().join("etc").join("passwd"), "root:x:0:0\n").unwrap();
        let t = task(json!(["contains:../etc/passwd:root"]));
        let c = validate_task_done(&t, &work_dir, "local");
        assert!(
            !c.ok,
            "contains:../etc/passwd:root must not treat a host file as session proof: {c:?}"
        );
        assert!(
            c.reasons.iter().any(|r| r.contains("../etc/passwd")),
            "fail reason must name the escaped rel: {c:?}"
        );
    }

    #[test]
    fn exists_backslash_or_nested_parent_segment_fails() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("result.md"), "Completed the marker file.\n").unwrap();
        std::fs::create_dir_all(dir.path().join("work")).unwrap();
        for rel in [r"..\etc\passwd", "foo/../../etc/passwd"] {
            let t = task(json!([format!("exists:{rel}")]));
            let c = validate_task_done(&t, dir.path(), "local");
            assert!(
                !c.ok,
                "exists:{rel} must not treat a parent segment as session proof: {c:?}"
            );
            assert!(
                c.reasons.iter().any(|r| r.contains(rel)),
                "fail reason must name the escaped rel {rel}: {c:?}"
            );
        }
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
        assert!(!c.ok, "{c:?}");
        assert!(
            c.reasons
                .iter()
                .any(|r| r.contains("hello.txt") && r.contains("hello")),
            "needle-miss must name the path and needle: {c:?}"
        );
    }

    #[test]
    fn contains_missing_path_fails() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("result.md"), "Completed the marker file.\n").unwrap();
        std::fs::create_dir_all(dir.path().join("work")).unwrap();
        let t = task(json!(["contains:hello.txt:hello"]));
        let c = validate_task_done(&t, dir.path(), "docker");
        assert!(!c.ok, "{c:?}");
        assert!(
            c.reasons.iter().any(|r| r.contains("does not exist")),
            "missing contains: path must say does not exist: {c:?}"
        );
    }

    #[test]
    fn contains_empty_needle_is_noop() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("result.md"), "Completed the marker file.\n").unwrap();
        std::fs::create_dir_all(dir.path().join("work")).unwrap();
        std::fs::write(dir.path().join("work").join("hello.txt"), "nope\n").unwrap();
        for line in ["contains:hello.txt:", "contains:hello.txt:   "] {
            let t = task(json!([line]));
            let c = validate_task_done(&t, dir.path(), "docker");
            assert!(c.ok, "empty needle {line} is a no-op: {c:?}");
        }
    }

    #[test]
    fn contains_needle_is_case_sensitive() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("result.md"), "Completed the marker file.\n").unwrap();
        std::fs::create_dir_all(dir.path().join("work")).unwrap();
        std::fs::write(dir.path().join("work").join("hello.txt"), "Hello").unwrap();
        let t = task(json!(["contains:hello.txt:hello"]));
        let c = validate_task_done(&t, dir.path(), "docker");
        assert!(!c.ok, "{c:?}");
        assert!(
            c.reasons.iter().any(|r| r.contains("does not contain")),
            "case-sensitive needle miss must say does not contain: {c:?}"
        );
    }

    #[test]
    fn generalist_default_acceptance_is_result_only() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("result.md"),
            "Completed the marker file.\n",
        )
        .unwrap();
        let mut t = task(json!([]));
        t.worker_profile = "generalist".into();
        let c = validate_task_done(&t, dir.path(), "process");
        assert!(
            c.ok,
            "generalist + long result.md must pass without work/: {c:?}"
        );
    }

    #[test]
    fn coding_empty_acceptance_fails_without_work_dir() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("result.md"),
            "Completed the marker file.\n",
        )
        .unwrap();
        let c = validate_task_done(&task(json!([])), dir.path(), "process");
        assert!(
            !c.ok,
            "coding + long result.md without work/ must fail: {c:?}"
        );
        assert!(
            c.reasons.iter().any(|r| r.contains("work")),
            "fail reason must mention work: {c:?}"
        );
    }

    #[test]
    fn review_or_testing_empty_acceptance_requires_pass_or_fail() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("result.md"),
            "Reviewed the change set thoroughly.\n",
        )
        .unwrap();
        for profile in ["reviewer", "testing"] {
            let mut t = task(json!([]));
            t.worker_profile = profile.into();
            let c = validate_task_done(&t, dir.path(), "process");
            assert!(!c.ok, "{profile} without pass/fail must fail: {c:?}");
            assert!(
                c.reasons.iter().any(|r| r.contains("must say pass or fail")),
                "{profile} fail reason must require pass or fail: {c:?}"
            );
        }
        std::fs::write(
            dir.path().join("result.md"),
            "Reviewed the change set thoroughly. PASS\n",
        )
        .unwrap();
        for profile in ["reviewer", "testing"] {
            let mut t = task(json!([]));
            t.worker_profile = profile.into();
            let c = validate_task_done(&t, dir.path(), "process");
            assert!(c.ok, "{profile} + PASS must pass: {c:?}");
        }
    }

    #[test]
    fn implement_alias_uses_coding_exists_work() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("result.md"),
            "Completed the marker file.\n",
        )
        .unwrap();
        let mut t = task(json!([]));
        t.worker_profile = "implementer".into();
        let c = validate_task_done(&t, dir.path(), "process");
        assert!(
            !c.ok,
            "implementer + long result.md without work/ must fail: {c:?}"
        );
        assert!(
            c.reasons.iter().any(|r| r.contains("work")),
            "fail reason must mention work: {c:?}"
        );
    }

    #[test]
    fn apply_task_terminal_only_from_assigned_in_progress_with_matching_claimed_session_id() {
        let claim = Uuid::new_v4();
        let other = Uuid::new_v4();
        let terminals = ["done_success", "done_failed", "blocked"];

        for status in ["assigned", "in_progress"] {
            let mut t = task(json!([]));
            t.status = status.into();
            t.claimed_session_id = Some(claim);
            for terminal in terminals {
                assert!(
                    can_apply_task_terminal(&t, claim, terminal),
                    "{status} + matching claim must allow {terminal}"
                );
            }
        }

        // Blocked HITL must not be overwritten — not even by the claiming session.
        let mut blocked = task(json!([]));
        blocked.status = "blocked".into();
        blocked.claimed_session_id = Some(claim);
        blocked.block_reason = Some("needs_approval".into());
        blocked.hitl_notes = Some("waiting on human".into());
        for terminal in terminals {
            assert!(
                !can_apply_task_terminal(&blocked, claim, terminal),
                "blocked HITL must not be overwritten by {terminal}"
            );
        }

        // Bounce clears the claim; a later session owns the card.
        let mut bounced = task(json!([]));
        bounced.status = "ready".into();
        bounced.claimed_session_id = None;
        for terminal in terminals {
            assert!(
                !can_apply_task_terminal(&bounced, claim, terminal),
                "stale {terminal} after bounce must not complete an unclaimed task"
            );
        }

        let mut reclaimed = task(json!([]));
        reclaimed.status = "in_progress".into();
        reclaimed.claimed_session_id = Some(other);
        for terminal in terminals {
            assert!(
                !can_apply_task_terminal(&reclaimed, claim, terminal),
                "stale {terminal} after bounce must not complete the wrong claim"
            );
        }
        for terminal in terminals {
            assert!(
                can_apply_task_terminal(&reclaimed, other, terminal),
                "the live claim may still apply {terminal}"
            );
        }

        for status in ["assigned", "in_progress"] {
            let mut t = task(json!([]));
            t.status = status.into();
            t.claimed_session_id = Some(claim);
            for terminal in terminals {
                assert!(
                    !can_apply_task_terminal(&t, other, terminal),
                    "{status} with a mismatched claim must not apply {terminal}"
                );
            }
        }

        for status in [
            "queued",
            "ready",
            "retry_wait",
            "done_success",
            "done_failed",
            "cancelled",
        ] {
            let mut t = task(json!([]));
            t.status = status.into();
            t.claimed_session_id = Some(claim);
            for terminal in terminals {
                assert!(
                    !can_apply_task_terminal(&t, claim, terminal),
                    "{status} must not accept apply_task_terminal {terminal}"
                );
            }
        }

        for status in ["assigned", "in_progress"] {
            let mut unclaimed = task(json!([]));
            unclaimed.status = status.into();
            unclaimed.claimed_session_id = None;
            assert!(
                !can_apply_task_terminal(&unclaimed, claim, "done_success"),
                "{status} with no claim must not complete"
            );
        }

        assert!(
            !can_apply_task_terminal(&reclaimed, other, "running"),
            "non-terminal worker status is not apply_task_terminal"
        );
        assert!(
            !can_apply_task_terminal(&reclaimed, other, "success"),
            "worker payload alias 'success' is not apply_task_terminal (must be done_success)"
        );
        assert!(
            !can_apply_task_terminal(&reclaimed, other, ""),
            "empty status is not apply_task_terminal"
        );
    }
}
