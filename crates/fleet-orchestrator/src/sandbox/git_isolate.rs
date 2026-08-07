//! Optional per-session git worktree (Vibe Kanban / Hermes pattern).
//! Control plane only: `FLEET_GIT_SOURCE` = local repo path or clone URL.
//! One bare cache per project; each sandbox gets `work_dir/repo`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

/// Attach an isolated checkout at `{session_work}/repo`.
/// Failures are soft — callers should log and continue without git.
pub async fn attach_worktree(
    project_dir: &Path,
    session_work: &Path,
    source: &str,
) -> Result<PathBuf, String> {
    let source = source.trim();
    if source.is_empty() {
        return Err("empty FLEET_GIT_SOURCE".into());
    }
    let dest = session_work.join("repo");
    if dest.exists() {
        return Ok(dest);
    }
    tokio::fs::create_dir_all(project_dir)
        .await
        .map_err(|e| format!("create project dir: {e}"))?;

    let cache = project_dir.join("source.git");
    ensure_bare_cache(&cache, source).await?;

    let out = Command::new("git")
        .args([
            "-C",
            &cache.to_string_lossy(),
            "worktree",
            "add",
            "--detach",
            &dest.to_string_lossy(),
        ])
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("git worktree add: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git worktree add failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(dest)
}

async fn ensure_bare_cache(cache: &Path, source: &str) -> Result<(), String> {
    if cache.join("HEAD").exists() {
        let _ = Command::new("git")
            .args(["-C", &cache.to_string_lossy(), "fetch", "--quiet", "--all"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        return Ok(());
    }
    if cache.exists() {
        tokio::fs::remove_dir_all(cache)
            .await
            .map_err(|e| format!("remove bad git cache: {e}"))?;
    }
    let out = Command::new("git")
        .args(["clone", "--bare", "--quiet", source, &cache.to_string_lossy()])
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("git clone --bare: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git clone --bare failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;

    #[tokio::test]
    async fn worktree_from_local_repo() {
        if StdCommand::new("git").arg("--version").output().is_err() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        assert!(StdCommand::new("git")
            .args(["-C", src.to_str().unwrap(), "init", "-q"])
            .status()
            .unwrap()
            .success());
        std::fs::write(src.join("README.md"), "hi").unwrap();
        let _ = StdCommand::new("git")
            .current_dir(&src)
            .args(["config", "user.email", "t@t.t"])
            .status();
        let _ = StdCommand::new("git")
            .current_dir(&src)
            .args(["config", "user.name", "t"])
            .status();
        assert!(StdCommand::new("git")
            .current_dir(&src)
            .args(["add", "."])
            .status()
            .unwrap()
            .success());
        assert!(StdCommand::new("git")
            .current_dir(&src)
            .args(["commit", "-qm", "init"])
            .status()
            .unwrap()
            .success());

        let project = root.path().join("proj");
        let session = root.path().join("sess");
        std::fs::create_dir_all(&session).unwrap();
        let dest = attach_worktree(&project, &session, src.to_str().unwrap())
            .await
            .expect("worktree");
        assert!(dest.join("README.md").exists());
    }
}
