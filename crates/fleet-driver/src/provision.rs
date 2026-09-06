//! Clone the project's git remote into the session work dir.
//!
//! Layout: `{work_dir}/repo` is the checkout; orch files stay at `{work_dir}`.
//! No repo configured → skip (mock / tests / no-op).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::process::Command;
use tracing::info;

/// Relative checkout directory under the session work dir.
pub const REPO_DIR_NAME: &str = "repo";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoToClone {
    pub url: String,
    pub git_ref: String,
}

impl RepoToClone {
    pub fn from_env_and_files(work_dir: &Path) -> Option<Self> {
        let env_url = std::env::var("FLEET_REPO_URL")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let env_ref = std::env::var("FLEET_GIT_REF")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if let Some(url) = env_url {
            return Some(Self {
                url,
                git_ref: env_ref.unwrap_or_else(|| "main".into()),
            });
        }
        load_workspace_json(work_dir)
    }
}

fn load_workspace_json(work_dir: &Path) -> Option<RepoToClone> {
    let raw = std::fs::read_to_string(work_dir.join("workspace.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let repo = v.get("repo")?;
    let url = repo.get("url")?.as_str()?.trim();
    if url.is_empty() {
        return None;
    }
    let git_ref = repo
        .get("ref")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("main");
    Some(RepoToClone {
        url: url.to_string(),
        git_ref: git_ref.to_string(),
    })
}

/// Ensure `{work_dir}/repo` is a checkout of `spec`. Returns the checkout path.
pub async fn ensure_repo(work_dir: &Path, spec: &RepoToClone) -> Result<PathBuf> {
    validate_clone_args(spec)?;
    let dest = work_dir.join(REPO_DIR_NAME);
    if dest.join(".git").is_dir() {
        fetch_and_checkout(&dest, spec).await?;
        return Ok(dest);
    }
    if dest.exists() {
        bail!(
            "checkout path {} exists but is not a git repo",
            dest.display()
        );
    }
    clone_fresh(&dest, spec).await?;
    Ok(dest)
}

fn validate_clone_args(spec: &RepoToClone) -> Result<()> {
    if spec.url.chars().any(|c| c.is_whitespace() || c == '\0') || spec.url.starts_with('-') {
        bail!("invalid FLEET_REPO_URL");
    }
    if spec.git_ref.is_empty()
        || spec.git_ref.chars().any(|c| c.is_whitespace() || c == '\0')
        || spec.git_ref.starts_with('-')
        || spec.git_ref.contains("..")
    {
        bail!("invalid FLEET_GIT_REF");
    }
    Ok(())
}

async fn clone_fresh(dest: &Path, spec: &RepoToClone) -> Result<()> {
    info!(url = %spec.url, git_ref = %spec.git_ref, dest = %dest.display(), "cloning project repo");
    let dest_str = dest.to_string_lossy().into_owned();
    let out = git_cmd()
        .args(["clone", "--", &spec.url, &dest_str])
        .output()
        .await
        .context("spawn git clone (is git installed?)")?;
    if !out.status.success() {
        bail!(
            "git clone failed: {}",
            tail_utf8(&out.stderr, &out.stdout, 800)
        );
    }
    checkout_ref(dest, &spec.git_ref).await
}

async fn fetch_and_checkout(dest: &Path, spec: &RepoToClone) -> Result<()> {
    info!(dest = %dest.display(), git_ref = %spec.git_ref, "updating existing checkout");
    let out = git_cmd()
        .current_dir(dest)
        .args(["fetch", "--all", "--tags"])
        .output()
        .await
        .context("git fetch")?;
    if !out.status.success() {
        bail!(
            "git fetch failed: {}",
            tail_utf8(&out.stderr, &out.stdout, 800)
        );
    }
    checkout_ref(dest, &spec.git_ref).await
}

async fn checkout_ref(repo: &Path, git_ref: &str) -> Result<()> {
    let out = git_cmd()
        .current_dir(repo)
        .args(["checkout", git_ref])
        .output()
        .await
        .context("git checkout")?;
    if !out.status.success() {
        bail!(
            "git checkout {git_ref} failed: {}",
            tail_utf8(&out.stderr, &out.stdout, 800)
        );
    }
    Ok(())
}

fn git_cmd() -> Command {
    let mut cmd = Command::new("git");
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "echo")
        .env("GCM_INTERACTIVE", "never");
    cmd
}

fn tail_utf8(stderr: &[u8], stdout: &[u8], max: usize) -> String {
    let mut s = String::from_utf8_lossy(stderr).into_owned();
    if s.trim().is_empty() {
        s = String::from_utf8_lossy(stdout).into_owned();
    }
    s.chars()
        .rev()
        .take(max)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

/// Wall-clock bound for clone + checkout.
pub fn clone_timeout(task_timeout: Duration) -> Duration {
    task_timeout
        .min(Duration::from_secs(300))
        .max(Duration::from_secs(15))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn init_src_repo() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let run = |args: &[&str]| {
            let st = std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .env("GIT_AUTHOR_NAME", "fleet")
                .env("GIT_AUTHOR_EMAIL", "fleet@example.test")
                .env("GIT_COMMITTER_NAME", "fleet")
                .env("GIT_COMMITTER_EMAIL", "fleet@example.test")
                .status()
                .unwrap();
            assert!(st.success(), "git {args:?}");
        };
        run(&["init"]);
        run(&["config", "user.email", "fleet@example.test"]);
        run(&["config", "user.name", "fleet"]);
        std::fs::write(root.join("hello.txt"), "from fixture repo\n").unwrap();
        run(&["add", "hello.txt"]);
        run(&["commit", "-m", "init"]);
        run(&["branch", "-M", "main"]);
        dir
    }

    #[tokio::test]
    async fn clones_local_file_url() {
        assert!(
            git_available(),
            "git must be on PATH for this unit test (install git; do not skip)"
        );
        let src = init_src_repo();
        let url = format!("file://{}", src.path().display());
        let dest_root = tempdir().unwrap();
        let spec = RepoToClone {
            url,
            git_ref: "main".into(),
        };
        let checkout = ensure_repo(dest_root.path(), &spec).await.unwrap();
        let body = std::fs::read_to_string(checkout.join("hello.txt")).unwrap();
        assert_eq!(body, "from fixture repo\n");
    }

    #[test]
    fn missing_workspace_json_is_none() {
        let dir = tempdir().unwrap();
        unsafe { std::env::remove_var("FLEET_REPO_URL") };
        assert!(RepoToClone::from_env_and_files(dir.path()).is_none());
    }

    #[test]
    fn workspace_json_invalid_json_is_none() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("workspace.json"), "not-json").unwrap();
        unsafe { std::env::remove_var("FLEET_REPO_URL") };
        assert!(RepoToClone::from_env_and_files(dir.path()).is_none());
    }

    #[test]
    fn workspace_json_missing_repo_or_url_key_is_none() {
        let dir = tempdir().unwrap();
        unsafe { std::env::remove_var("FLEET_REPO_URL") };
        std::fs::write(dir.path().join("workspace.json"), "{}").unwrap();
        assert!(RepoToClone::from_env_and_files(dir.path()).is_none());
        std::fs::write(dir.path().join("workspace.json"), r#"{"repo":{}}"#).unwrap();
        assert!(RepoToClone::from_env_and_files(dir.path()).is_none());
    }
}
