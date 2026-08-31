//! Control-plane integration bridge used by workers and run write-back.

use async_trait::async_trait;
use fleet_db::PgPool;
use fleet_integrations::proxy::{self, ProxyCaller, ProxyRequest};
use fleet_integrations::CredentialsKey;
use fleet_orchestrator::sandbox::io::{IntegrationInvoker, IntegrationResult};
use serde_json::Value;
use uuid::Uuid;

#[cfg(test)]
use std::io;
#[cfg(test)]
use std::path::{Component, Path, PathBuf};

/// Resolve a task-relative work file so it cannot leave `work_dir`.
///
/// Production validate-done lives in the orchestrator. This copy is the
/// crate's unit lock for NUL / `..` / symlink escape.
#[cfg(test)]
fn resolve_work_path(work_dir: &Path, rel: &str) -> io::Result<PathBuf> {
    if rel.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "work path contains NUL",
        ));
    }
    // Treat `\` as a separator too so `..\etc\passwd` cannot sneak through on Unix.
    if rel.split(['/', '\\']).any(|p| p == "..") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "work path must not contain '..'",
        ));
    }

    let normalized = rel.replace('\\', "/");
    let path = Path::new(&normalized);
    if path.is_absolute() || normalized.starts_with('/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "work path must be relative",
        ));
    }
    for c in path.components() {
        match c {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "work path escapes work dir",
                ));
            }
        }
    }

    let root = work_dir.canonicalize()?;
    let joined = root.join(path);
    if joined.exists() {
        let canon = joined.canonicalize()?;
        if !canon.starts_with(&root) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "work path escapes work dir",
            ));
        }
        return Ok(canon);
    }

    let mut ancestor = joined.as_path();
    while !ancestor.exists() {
        ancestor = ancestor.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "work path escapes work dir")
        })?;
    }
    let ancestor = ancestor.canonicalize()?;
    if !ancestor.starts_with(&root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "work path escapes work dir",
        ));
    }
    Ok(joined)
}

#[derive(Clone)]
pub struct ControlPlaneIo {
    pub db: PgPool,
    pub key: CredentialsKey,
}

#[async_trait]
impl IntegrationInvoker for ControlPlaneIo {
    async fn invoke(
        &self,
        workspace_id: Uuid,
        provider: String,
        operation: String,
        args: Value,
    ) -> IntegrationResult {
        let res = proxy::invoke(
            &self.db,
            &self.key,
            ProxyRequest {
                workspace_id,
                provider,
                operation,
                args,
                caller: ProxyCaller::System,
            },
        )
        .await;
        IntegrationResult {
            ok: res.ok,
            summary: res.summary,
            data: res.data,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_work_path_rejects_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        for rel in [
            "../etc/passwd",
            "ok/../../../etc/passwd",
            r"..\etc\passwd",
            "foo/../../bar",
            "nested/../outside",
        ] {
            assert!(
                resolve_work_path(dir.path(), rel).is_err(),
                "must reject {rel:?}"
            );
        }
        assert!(
            resolve_work_path(dir.path(), "ok/file.txt").is_ok(),
            "relative path under the work dir must stay allowed"
        );
    }

    #[test]
    fn resolve_work_path_rejects_absolute_paths() {
        let dir = tempfile::tempdir().unwrap();
        for rel in ["/etc/passwd", "/tmp/x", "//etc/passwd"] {
            assert!(
                resolve_work_path(dir.path(), rel).is_err(),
                "must reject absolute {rel:?}"
            );
        }
    }

    #[test]
    fn resolve_work_path_rejects_nul() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_work_path(dir.path(), "foo\0bar").is_err());
        assert!(resolve_work_path(dir.path(), "\0etc/passwd").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn resolve_work_path_rejects_symlink_escape() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "x").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("out")).unwrap();
        assert!(
            resolve_work_path(dir.path(), "out/secret").is_err(),
            "symlink that leaves the work dir must fail closed"
        );
        assert!(
            resolve_work_path(dir.path(), "out").is_err(),
            "symlink to a dir outside the work dir must fail closed"
        );
    }

    #[test]
    fn resolve_work_path_keeps_relative_files_under_work_dir() {
        let dir = tempfile::tempdir().unwrap();
        let inside = dir.path().join("notes.md");
        std::fs::write(&inside, "ok").unwrap();
        let got = resolve_work_path(dir.path(), "notes.md").expect("in-tree file");
        assert_eq!(got, inside.canonicalize().unwrap());
        assert!(got.starts_with(dir.path().canonicalize().unwrap()));
    }
}
