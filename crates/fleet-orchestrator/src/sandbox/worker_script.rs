//! Task files + worker script for process/docker sandboxes.

use std::path::Path;

use super::{SandboxError, SpawnRequest};

pub async fn materialize_workspace(req: &SpawnRequest) -> Result<(), SandboxError> {
    tokio::fs::create_dir_all(&req.work_dir)
        .await
        .map_err(|e| SandboxError::Spawn(format!("create work_dir: {e}")))?;

    let title = req.task_title.as_deref().unwrap_or("untitled");
    let summary = req.task_summary.as_deref().unwrap_or("");
    let spec = req.task_spec.as_deref().unwrap_or("");

    let task_md = format!(
        "# {title}\n\n\
         **plan_task_id:** `{}`\n\
         **profile:** `{}`\n\n\
         ## Summary\n\n{summary}\n\n\
         ## Spec\n\n{spec}\n",
        req.plan_task_id, req.worker_profile
    );
    write_file(req.work_dir.join("task.md"), &task_md).await?;

    let task_json = serde_json::json!({
        "session_id": req.session_id,
        "project_id": req.project_id,
        "run_id": req.run_id,
        "board_task_id": req.board_task_id,
        "plan_task_id": req.plan_task_id,
        "worker_profile": req.worker_profile,
        "title": title,
        "summary": summary,
        "spec_md": spec,
    });
    write_file(
        req.work_dir.join("task.json"),
        &serde_json::to_string_pretty(&task_json).unwrap_or_else(|_| "{}".into()),
    )
    .await?;

    let cmds = extract_bash_blocks(spec);
    if !cmds.is_empty() {
        let mut body = String::from("#!/usr/bin/env bash\nset -euo pipefail\n\n");
        body.push_str("# Auto-extracted from task spec ```bash fences\n");
        for (i, block) in cmds.iter().enumerate() {
            body.push_str(&format!("\n# --- block {} ---\n", i + 1));
            body.push_str(block);
            if !block.ends_with('\n') {
                body.push('\n');
            }
        }
        write_file(req.work_dir.join("commands.sh"), &body).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = req.work_dir.join("commands.sh");
            let mut perms = tokio::fs::metadata(&path)
                .await
                .map_err(|e| SandboxError::Spawn(format!("stat commands.sh: {e}")))?
                .permissions();
            perms.set_mode(0o755);
            tokio::fs::set_permissions(&path, perms)
                .await
                .map_err(|e| SandboxError::Spawn(format!("chmod commands.sh: {e}")))?;
        }
    }

    let worker = WORKER_SH;
    write_file(req.work_dir.join("fleet_worker.sh"), worker).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = req.work_dir.join("fleet_worker.sh");
        let mut perms = tokio::fs::metadata(&path)
            .await
            .map_err(|e| SandboxError::Spawn(format!("stat fleet_worker.sh: {e}")))?
            .permissions();
        perms.set_mode(0o755);
        tokio::fs::set_permissions(&path, perms)
            .await
            .map_err(|e| SandboxError::Spawn(format!("chmod fleet_worker.sh: {e}")))?;
    }

    Ok(())
}

async fn write_file(path: impl AsRef<Path>, contents: &str) -> Result<(), SandboxError> {
    tokio::fs::write(path.as_ref(), contents)
        .await
        .map_err(|e| SandboxError::Spawn(format!("write {}: {e}", path.as_ref().display())))
}

pub fn extract_bash_blocks(spec: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut lines = spec.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        let open = trimmed == "```bash"
            || trimmed == "```sh"
            || trimmed == "```shell"
            || trimmed.starts_with("```bash ")
            || trimmed.starts_with("```sh ");
        if !open {
            continue;
        }
        let mut block = String::new();
        for inner in lines.by_ref() {
            if inner.trim() == "```" {
                break;
            }
            block.push_str(inner);
            block.push('\n');
        }
        let block = block.trim().to_string();
        if !block.is_empty() {
            out.push(block);
        }
    }
    out
}

const WORKER_SH: &str = r#"#!/usr/bin/env bash
set -uo pipefail

WORK="${FLEET_WORK_DIR:-.}"
cd "$WORK" || exit 91
if [[ -d "$WORK/repo" ]]; then
  cd "$WORK/repo" || exit 91
fi

echo "running" > "$WORK/.fleet_status"
echo "started_at=$(date -Iseconds 2>/dev/null || date)" > "$WORK/.fleet_meta"
{
  echo "=== fleet worker ==="
  echo "session=${FLEET_SESSION_ID:-}"
  echo "project=${FLEET_PROJECT_ID:-}"
  echo "run=${FLEET_RUN_ID:-}"
  echo "task=${FLEET_PLAN_TASK_ID:-}"
  echo "profile=${FLEET_WORKER_PROFILE:-}"
  echo "work_dir=$WORK"
  echo "pwd=$(pwd)"
  echo "date=$(date -Iseconds 2>/dev/null || date)"
  echo "=== task title ==="
  head -n 5 "$WORK/task.md" 2>/dev/null || true
} | tee -a "$WORK/stdout.log"

EXIT=0
if [[ -f "$WORK/commands.sh" ]]; then
  echo "=== running commands.sh ===" | tee -a "$WORK/stdout.log"
  # shellcheck disable=SC1091
  bash "$WORK/commands.sh" >>"$WORK/stdout.log" 2>>"$WORK/stderr.log"
  EXIT=$?
else
  echo "=== no commands.sh; default probe ===" | tee -a "$WORK/stdout.log"
  {
    uname -a 2>/dev/null || true
    command -v bash >/dev/null && bash --version | head -n1
    ls -la
    # Prove isolation: no integration secrets in env
    if env | grep -Eiq 'ACCESS_TOKEN|REFRESH_TOKEN|GITHUB_TOKEN|JIRA_.*SECRET'; then
      echo "SECURITY: unexpected secret-like env var" >&2
      EXIT=92
    else
      echo "env_ok: no oauth tokens in sandbox environment"
    fi
  } >>"$WORK/stdout.log" 2>>"$WORK/stderr.log"
fi

echo "$EXIT" > "$WORK/.fleet_exit"
if [[ "$EXIT" -eq 0 ]]; then
  echo "success" > "$WORK/.fleet_status"
else
  echo "failed" > "$WORK/.fleet_status"
fi
echo "finished_at=$(date -Iseconds 2>/dev/null || date)" >> "$WORK/.fleet_meta"
echo "=== done exit=$EXIT ===" | tee -a "$WORK/stdout.log"
exit "$EXIT"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_bash_fences() {
        let spec = r#"
Do work.

```bash
echo hello
pwd
```

More text.

```sh
ls -la
```
"#;
        let blocks = extract_bash_blocks(spec);
        assert_eq!(blocks.len(), 2);
        assert!(blocks[0].contains("echo hello"));
        assert!(blocks[1].contains("ls -la"));
    }

    #[test]
    fn worker_runs_commands_from_work_root() {
        assert!(WORKER_SH.contains(r#"bash "$WORK/commands.sh""#));
        assert!(WORKER_SH.contains(r#"head -n 5 "$WORK/task.md""#));
        assert!(WORKER_SH.contains(r#""$WORK/.fleet_exit""#));
    }
}
