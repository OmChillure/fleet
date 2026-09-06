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
