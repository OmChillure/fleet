//! Build the agent prompt from session work-dir files.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskContext {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub spec_md: String,
    #[serde(default)]
    pub acceptance: Vec<String>,
    #[serde(default)]
    pub plan_task_id: String,
    #[serde(default)]
    pub worker_profile: String,
    #[serde(default)]
    pub timeout_s: Option<u64>,
}

impl TaskContext {
    pub fn load(work_dir: &Path) -> Self {
        let mut ctx = TaskContext::default();
        let task_path = work_dir.join("task.json");
        if let Ok(raw) = std::fs::read_to_string(&task_path)
            && let Ok(parsed) = serde_json::from_str::<TaskContext>(&raw)
        {
            ctx = parsed;
        }
        if ctx.spec_md.is_empty()
            && let Ok(spec) = std::fs::read_to_string(work_dir.join("spec.md"))
        {
            ctx.spec_md = spec;
        }
        if ctx.plan_task_id.is_empty()
            && let Ok(id) = std::fs::read_to_string(work_dir.join("plan_task_id.txt"))
        {
            ctx.plan_task_id = id.trim().to_string();
        }
        ctx
    }
}

/// Prefer orch-written `prompt.md`; otherwise compose from [`TaskContext`].
pub fn load_prompt(work_dir: &Path) -> String {
    let prompt_path = work_dir.join("prompt.md");
    if let Ok(p) = std::fs::read_to_string(&prompt_path)
        && !p.trim().is_empty()
    {
        return p;
    }
    compose_prompt(&TaskContext::load(work_dir))
}

pub fn compose_prompt(ctx: &TaskContext) -> String {
    let mut out = String::new();
    if !ctx.title.trim().is_empty() {
        out.push_str("# ");
        out.push_str(ctx.title.trim());
        out.push_str("\n\n");
    }
    if !ctx.summary.trim().is_empty() {
        out.push_str(ctx.summary.trim());
        out.push_str("\n\n");
    }
    if !ctx.spec_md.trim().is_empty() {
        out.push_str(ctx.spec_md.trim());
        out.push_str("\n\n");
    }
    if !ctx.acceptance.is_empty() {
        out.push_str("## Acceptance\n\n");
        for line in &ctx.acceptance {
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
        if !ctx.plan_task_id.is_empty() {
            return format!(
                "Complete plan task `{}`. Inspect the workspace and finish the work.",
                ctx.plan_task_id
            );
        }
        return "Inspect this workspace and complete the assigned task.".into();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn compose_includes_title_spec_acceptance() {
        let ctx = TaskContext {
            title: "Add marker".into(),
            summary: "Tiny change".into(),
            spec_md: "Write `hello.txt`.".into(),
            acceptance: vec!["file exists".into(), "contains hello".into()],
            ..Default::default()
        };
        let p = compose_prompt(&ctx);
        assert!(p.contains("# Add marker"));
        assert!(p.contains("Tiny change"));
        assert!(p.contains("Write `hello.txt`."));
        assert!(p.contains("- file exists"));
        assert!(p.contains("- contains hello"));
    }

    #[test]
    fn load_prefers_prompt_md() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("prompt.md"), "FROM PROMPT FILE").unwrap();
        std::fs::write(dir.path().join("spec.md"), "FROM SPEC").unwrap();
        assert_eq!(load_prompt(dir.path()).trim(), "FROM PROMPT FILE");
    }

    #[test]
    fn load_falls_back_to_spec() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("spec.md"), "do the thing").unwrap();
        let p = load_prompt(dir.path());
        assert!(p.contains("do the thing"));
    }
}
