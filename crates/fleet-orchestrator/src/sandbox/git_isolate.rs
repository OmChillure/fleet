//! Optional per-session git worktree (Vibe Kanban / Hermes pattern).
//! Control plane only: `FLEET_GIT_SOURCE` = local repo path or clone URL.
//! Bare cache per remote URL (`source.git`, then `source-<sha>.git` if Settings
//! changes so already-spawned worktrees keep the previous cache).
//! Each sandbox gets `work_dir/repo`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;

use sha2::{Digest, Sha256};
use tokio::process::Command;
use tokio::sync::Mutex;

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

    let cache = {
        let _guard = cache_lock().lock().await;
        let cache = resolve_cache(project_dir, source).await;
        ensure_bare_cache(&cache, source).await?;
        cache
    };

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

fn cache_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Remove this session's worktree from the bare cache it was attached to, then prune.
pub async fn detach_worktree(project_dir: &Path, session_work: &Path) {
    let dest = session_work.join("repo");
    if !dest.exists() {
        return;
    }
    let cache = match git_common_dir(&dest).await {
        Some(dir) if dir.join("HEAD").exists() => dir,
        _ => {
            let fallback = project_dir.join("source.git");
            if !fallback.join("HEAD").exists() {
                return;
            }
            fallback
        }
    };
    let _guard = cache_lock().lock().await;
    let dest_s = dest.to_string_lossy().into_owned();
    let cache_s = cache.to_string_lossy().into_owned();
    let _ = Command::new("git")
        .args(["-C", &cache_s, "worktree", "remove", "--force", &dest_s])
        .stdin(Stdio::null())
        .output()
        .await;
    let _ = Command::new("git")
        .args(["-C", &cache_s, "worktree", "prune"])
        .stdin(Stdio::null())
        .output()
        .await;
}

fn normalize_git_source(source: &str) -> String {
    let s = source.trim();
    let s = s.strip_prefix("file://").unwrap_or(s);
    let s = s.trim_end_matches('/');
    let path = Path::new(s);
    if let Ok(canon) = path.canonicalize() {
        return canon.to_string_lossy().into_owned();
    }
    s.to_string()
}

fn source_cache_key(source: &str) -> String {
    let mut h = Sha256::new();
    h.update(normalize_git_source(source).as_bytes());
    let digest = hex::encode(h.finalize());
    digest[..16].to_string()
}

async fn cache_origin_url(cache: &Path) -> Option<String> {
    let out = Command::new("git")
        .args([
            "-C",
            &cache.to_string_lossy(),
            "remote",
            "get-url",
            "origin",
        ])
        .stdin(Stdio::null())
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if url.is_empty() { None } else { Some(url) }
}

async fn cache_origin_matches(cache: &Path, source: &str) -> bool {
    let Some(origin) = cache_origin_url(cache).await else {
        return false;
    };
    normalize_git_source(&origin) == normalize_git_source(source)
}

/// Prefer the well-known `source.git` when it is this remote. A later Settings
/// URL change must not fetch into that cache — existing worktrees still need it.
async fn resolve_cache(project_dir: &Path, source: &str) -> PathBuf {
    let default = project_dir.join("source.git");
    if !default.join("HEAD").exists() || cache_origin_matches(&default, source).await {
        default
    } else {
        project_dir.join(format!("source-{}.git", source_cache_key(source)))
    }
}

async fn git_common_dir(repo: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .args([
            "-C",
            &repo.to_string_lossy(),
            "rev-parse",
            "--git-common-dir",
        ])
        .stdin(Stdio::null())
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if raw.is_empty() {
        return None;
    }
    let p = PathBuf::from(&raw);
    let abs = if p.is_absolute() { p } else { repo.join(p) };
    tokio::fs::canonicalize(&abs).await.ok().or(Some(abs))
}

async fn ensure_bare_cache(cache: &Path, source: &str) -> Result<(), String> {
    if cache.join("HEAD").exists() {
        if !cache_origin_matches(cache, source).await {
            return Err(format!(
                "git cache {} is not {source}; refusing to reuse another remote's objects",
                cache.display()
            ));
        }
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
        .args([
            "clone",
            "--bare",
            "--quiet",
            source,
            &cache.to_string_lossy(),
        ])
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

    fn require_git() {
        assert!(
            StdCommand::new("git").arg("--version").output().is_ok(),
            "git is required for git-isolate tests (install git; see install-full-suite-deps.sh)"
        );
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = StdCommand::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("git {}: {e}", args.join(" ")));
        assert!(
            out.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_commit_repo(path: &Path, files: &[(&str, &str)]) {
        std::fs::create_dir_all(path).unwrap();
        git(path, &["init", "-q"]);
        git(path, &["config", "user.email", "t@t.t"]);
        git(path, &["config", "user.name", "t"]);
        for (rel, body) in files {
            let dest = path.join(rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&dest, body).unwrap();
        }
        git(path, &["add", "."]);
        git(path, &["commit", "-qm", "init"]);
    }

    fn assert_inside_work_tree(repo: &Path, why: &str) {
        let out = StdCommand::new("git")
            .args([
                "-C",
                repo.to_str().unwrap(),
                "rev-parse",
                "--is-inside-work-tree",
            ])
            .output()
            .unwrap_or_else(|e| panic!("git rev-parse {why}: {e}"));
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "true",
            "{why}: already-spawned session worktree must stay a valid git checkout (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Docs §16: Settings git remote set after a run started — already-spawned
    /// sessions keep their workdir. New spawns clone the new URL.
    ///
    /// Uses the same layout `schedule_and_spawn` writes:
    /// `{data}/projects/{project}/sessions/{session}/repo`.
    #[tokio::test]
    async fn already_spawned_keep_workdir_on_remote_change() {
        require_git();
        let root = tempfile::tempdir().unwrap();
        let old_src = root.path().join("remote-old");
        let new_src = root.path().join("remote-new");
        init_commit_repo(
            &old_src,
            &[("FROM_OLD.txt", "alpha-remote"), ("README.md", "old")],
        );
        init_commit_repo(
            &new_src,
            &[("FROM_NEW.txt", "beta-remote"), ("README.md", "new")],
        );

        let project_id = "11111111-1111-1111-1111-111111111111";
        let spawned_sid = "22222222-2222-2222-2222-222222222222";
        let fresh_sid = "33333333-3333-3333-3333-333333333333";
        let project_dir = root.path().join("projects").join(project_id);
        let spawned_work = project_dir.join("sessions").join(spawned_sid);
        let fresh_work = project_dir.join("sessions").join(fresh_sid);
        std::fs::create_dir_all(&spawned_work).unwrap();
        std::fs::create_dir_all(&fresh_work).unwrap();

        let spawned_repo = attach_worktree(&project_dir, &spawned_work, old_src.to_str().unwrap())
            .await
            .expect("first spawn clones the Settings remote at spawn time");
        assert_eq!(spawned_repo, spawned_work.join("repo"));
        assert_eq!(
            std::fs::read_to_string(spawned_repo.join("FROM_OLD.txt")).unwrap(),
            "alpha-remote"
        );
        assert!(
            !spawned_repo.join("FROM_NEW.txt").exists(),
            "first spawn must not contain the later Settings remote"
        );
        std::fs::write(spawned_repo.join("AGENT_EDIT.txt"), "in-flight work").unwrap();
        assert_inside_work_tree(&spawned_repo, "after first spawn");

        // Settings git remote changed after this session was already spawned.
        let again = attach_worktree(&project_dir, &spawned_work, new_src.to_str().unwrap())
            .await
            .expect("re-attach of an already-spawned session must succeed without rewrite");
        assert_eq!(again, spawned_work.join("repo"));
        assert_eq!(
            std::fs::read_to_string(again.join("FROM_OLD.txt")).unwrap(),
            "alpha-remote",
            "already-spawned session must keep the workdir cloned at spawn"
        );
        assert_eq!(
            std::fs::read_to_string(again.join("AGENT_EDIT.txt")).unwrap(),
            "in-flight work",
            "already-spawned session workdir must not be rewritten when Settings git remote changes"
        );
        assert!(
            !again.join("FROM_NEW.txt").exists(),
            "changing Settings git remote must not re-clone into an already-spawned session"
        );
        assert_inside_work_tree(&again, "after Settings remote change");
        // Even an unreachable new remote must not consult the network or rewrite.
        let still = attach_worktree(
            &project_dir,
            &spawned_work,
            "https://example.invalid/does-not-exist.git",
        )
        .await
        .expect("already-spawned workdir must be left alone even if the new remote is invalid");
        assert_eq!(
            std::fs::read_to_string(still.join("AGENT_EDIT.txt")).unwrap(),
            "in-flight work"
        );

        let fresh_repo = attach_worktree(&project_dir, &fresh_work, new_src.to_str().unwrap())
            .await
            .expect("new spawn must clone the current Settings git remote");
        assert_eq!(fresh_repo, fresh_work.join("repo"));
        let fresh_listing: Vec<_> = std::fs::read_dir(&fresh_repo)
            .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.file_name())).collect())
            .unwrap_or_default();
        let from_new =
            std::fs::read_to_string(fresh_repo.join("FROM_NEW.txt")).unwrap_or_else(|e| {
                panic!(
                    "new spawn must clone the new Settings URL (FROM_NEW.txt missing: {e}); \
                 checkout contains {fresh_listing:?} — must not reuse the previous remote's cache"
                )
            });
        assert_eq!(
            from_new, "beta-remote",
            "new spawn must clone the new Settings URL, not the stale per-project cache"
        );
        assert!(
            !fresh_repo.join("FROM_OLD.txt").exists(),
            "new spawn must not keep the previous Settings remote checkout; got {fresh_listing:?}"
        );
        assert_eq!(
            std::fs::read_to_string(fresh_repo.join("README.md")).unwrap(),
            "new"
        );
        assert_eq!(
            std::fs::read_to_string(spawned_work.join("repo/FROM_OLD.txt")).unwrap(),
            "alpha-remote",
            "new spawn must not mutate the already-spawned session workdir"
        );
        assert_eq!(
            std::fs::read_to_string(spawned_work.join("repo/AGENT_EDIT.txt")).unwrap(),
            "in-flight work",
            "new spawn must not wipe in-flight edits in an already-spawned workdir"
        );
        assert!(
            !spawned_work.join("repo/FROM_NEW.txt").exists(),
            "new spawn must not re-clone the new Settings URL into the already-spawned workdir"
        );
        assert_inside_work_tree(
            &spawned_work.join("repo"),
            "after a later spawn cloned the new Settings remote",
        );
    }

    /// Docs §16: Settings git remote cleared → new tasks skip clone
    /// (prompt-only). Must not clone the previous remote or reuse its
    /// per-project `source.git` cache.
    #[tokio::test]
    async fn new_spawn_skips_clone_when_remote_cleared() {
        require_git();
        let root = tempfile::tempdir().unwrap();
        let old_src = root.path().join("remote-old");
        init_commit_repo(
            &old_src,
            &[("FROM_OLD.txt", "alpha-remote"), ("README.md", "old")],
        );

        let project_id = "11111111-1111-1111-1111-111111111111";
        let spawned_sid = "22222222-2222-2222-2222-222222222222";
        let fresh_sid = "33333333-3333-3333-3333-333333333333";
        let whitespace_sid = "44444444-4444-4444-4444-444444444444";
        let control_sid = "55555555-5555-5555-5555-555555555555";
        let project_dir = root.path().join("projects").join(project_id);
        let spawned_work = project_dir.join("sessions").join(spawned_sid);
        let fresh_work = project_dir.join("sessions").join(fresh_sid);
        let whitespace_work = project_dir.join("sessions").join(whitespace_sid);
        let control_work = project_dir.join("sessions").join(control_sid);
        std::fs::create_dir_all(&spawned_work).unwrap();
        std::fs::create_dir_all(&fresh_work).unwrap();
        std::fs::create_dir_all(&whitespace_work).unwrap();
        std::fs::create_dir_all(&control_work).unwrap();

        let spawned_repo = attach_worktree(&project_dir, &spawned_work, old_src.to_str().unwrap())
            .await
            .expect("first spawn clones the Settings remote at spawn time");
        assert_eq!(
            std::fs::read_to_string(spawned_repo.join("FROM_OLD.txt")).unwrap(),
            "alpha-remote"
        );
        std::fs::write(spawned_repo.join("AGENT_EDIT.txt"), "in-flight work").unwrap();
        assert!(
            project_dir.join("source.git").join("HEAD").exists(),
            "previous remote must leave a per-project cache the later skip-clone must not reuse"
        );

        // Settings git remote cleared — new session must not clone.
        let skipped = attach_worktree(&project_dir, &fresh_work, "").await;
        assert!(
            skipped.is_err(),
            "cleared Settings remote must skip clone, not attach leftover source.git: {skipped:?}"
        );
        assert!(
            !fresh_work.join("repo").exists(),
            "new spawn after Settings clear must not create repo/ from the previous remote"
        );
        assert!(
            !fresh_work.join("repo/FROM_OLD.txt").exists(),
            "new spawn after Settings clear must not check out FROM_OLD from leftover cache"
        );
        let whitespace = attach_worktree(&project_dir, &whitespace_work, "   ").await;
        assert!(
            whitespace.is_err(),
            "whitespace Settings URL is a clear and must skip clone: {whitespace:?}"
        );
        assert!(
            !whitespace_work.join("repo").exists(),
            "whitespace-cleared remote must not create repo/"
        );

        assert_eq!(
            std::fs::read_to_string(spawned_work.join("repo/FROM_OLD.txt")).unwrap(),
            "alpha-remote",
            "skipping clone on a new session must not mutate the already-spawned workdir"
        );
        assert_eq!(
            std::fs::read_to_string(spawned_work.join("repo/AGENT_EDIT.txt")).unwrap(),
            "in-flight work",
            "skipping clone on a new session must not wipe in-flight edits"
        );
        assert_inside_work_tree(
            &spawned_work.join("repo"),
            "already-spawned worktree after Settings remote was cleared",
        );

        let control = attach_worktree(&project_dir, &control_work, old_src.to_str().unwrap())
            .await
            .expect("control: a still-set remote must still clone (not a tautology that every attach skips)");
        assert_eq!(
            std::fs::read_to_string(control.join("FROM_OLD.txt")).unwrap(),
            "alpha-remote"
        );
        assert!(
            !fresh_work.join("repo").exists(),
            "control clone must not backfill the skipped new-spawn workdir"
        );
    }

    #[tokio::test]
    async fn worktree_from_local_repo() {
        if StdCommand::new("git").arg("--version").output().is_err() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        assert!(
            StdCommand::new("git")
                .args(["-C", src.to_str().unwrap(), "init", "-q"])
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(src.join("README.md"), "hi").unwrap();
        let _ = StdCommand::new("git")
            .current_dir(&src)
            .args(["config", "user.email", "t@t.t"])
            .status();
        let _ = StdCommand::new("git")
            .current_dir(&src)
            .args(["config", "user.name", "t"])
            .status();
        assert!(
            StdCommand::new("git")
                .current_dir(&src)
                .args(["add", "."])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            StdCommand::new("git")
                .current_dir(&src)
                .args(["commit", "-qm", "init"])
                .status()
                .unwrap()
                .success()
        );

        let project = root.path().join("proj");
        let session = root.path().join("sess");
        std::fs::create_dir_all(&session).unwrap();
        let dest = attach_worktree(&project, &session, src.to_str().unwrap())
            .await
            .expect("worktree");
        assert!(dest.join("README.md").exists());
    }
}
