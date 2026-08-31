//! On-disk layout: `$HOME/.config/fleet-host/{config.toml,machine.id,machine.token,run.lock}`.

use std::env;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::Context;

#[cfg(unix)]
use std::os::unix::io::AsRawFd;

/// Config directory. `FLEET_HOST_CONFIG_DIR` wins (tests / unusual homes).
pub fn config_dir() -> PathBuf {
    if let Ok(p) = env::var("FLEET_HOST_CONFIG_DIR") {
        let p = p.trim();
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    home_dir().join(".config").join("fleet-host")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn token_path() -> PathBuf {
    config_dir().join("machine.token")
}

pub fn machine_id_path() -> PathBuf {
    config_dir().join("machine.id")
}

/// Exclusive lock file for this config dir (`run.lock` next to `machine.id`).
pub fn run_lock_path() -> PathBuf {
    run_lock_path_in(&config_dir())
}

fn run_lock_path_in(dir: &Path) -> PathBuf {
    dir.join("run.lock")
}

/// Held `run.lock`. Unlocks on drop so a restart after crash/exit can start.
#[derive(Debug)]
pub struct RunLock {
    file: File,
}

impl Drop for RunLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

/// One `fleet-host run` per config dir. A second process would share the
/// machine token / `host_id` and pick docker vs kube nondeterministically.
pub fn acquire_run_lock() -> anyhow::Result<RunLock> {
    acquire_run_lock_in(&config_dir())
}

/// Same as [`acquire_run_lock`] against `dir` (tests; no env mutation).
fn acquire_run_lock_in(dir: &Path) -> anyhow::Result<RunLock> {
    let path = run_lock_path_in(dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let mut opts = OpenOptions::new();
    opts.create(true).write(true).truncate(false);
    // Children spawned by parallel tests must not inherit this fd — flock
    // is held until every copy of the fd is closed.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_CLOEXEC);
    }
    let file = opts
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    #[cfg(unix)]
    {
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            let os = std::io::Error::last_os_error();
            anyhow::bail!(
                "fleet-host already running (lock {}): {os}",
                path.display()
            );
        }
    }
    #[cfg(windows)]
    {
        let _ = &file;
    }
    Ok(RunLock { file })
}

/// Stable box id. Created once; survives unpair (token wipe) so re-pair
/// reclaims the same worker row.
pub fn load_or_create_machine_id() -> anyhow::Result<uuid::Uuid> {
    load_or_create_machine_id_in(&config_dir())
}

/// Same as [`load_or_create_machine_id`] against `dir` (tests; no env mutation).
fn load_or_create_machine_id_in(dir: &Path) -> anyhow::Result<uuid::Uuid> {
    use std::fs;
    let path = dir.join("machine.id");
    if let Ok(raw) = fs::read_to_string(&path) {
        if let Ok(id) = uuid::Uuid::parse_str(raw.trim()) {
            return Ok(id);
        }
    }
    let id = uuid::Uuid::now_v7();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, format!("{id}\n"))?;
    // Identity is durable across unpair; do not leave it world-readable (umask 022 → 644).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&path)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&path, perms)?;
    }
    Ok(id)
}

pub fn home_dir() -> PathBuf {
    if let Ok(h) = env::var("HOME") {
        let h = h.trim();
        if !h.is_empty() {
            return PathBuf::from(h);
        }
    }
    #[cfg(windows)]
    {
        if let Ok(p) = env::var("USERPROFILE") {
            let p = p.trim();
            if !p.is_empty() {
                return PathBuf::from(p);
            }
        }
    }
    PathBuf::from(".")
}

/// Expand a leading `~/` using [`home_dir`].
pub fn expand_tilde(p: &str) -> PathBuf {
    if p == "~" {
        return home_dir();
    }
    if let Some(rest) = p.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(p)
}

pub fn default_work_dir_string() -> String {
    "~/.local/share/fleet-host/work".into()
}

/// catalog:unit-machine-id-persist — host `machine.id` is written once at
/// 0600 and reused after token wipe / surrounding whitespace / update
/// dest-dir swap so unpair+re-pair reclaims the same worker row.
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn first_create_and_second_load_return_same_uuid() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_create_machine_id_in(dir.path()).unwrap();
        let second = load_or_create_machine_id_in(dir.path()).unwrap();
        assert_eq!(first, second);
        let on_disk = fs::read_to_string(dir.path().join("machine.id")).unwrap();
        assert_eq!(on_disk, format!("{first}\n"));
    }

    #[test]
    fn deleting_machine_token_does_not_change_machine_id() {
        let dir = tempfile::tempdir().unwrap();
        let id = load_or_create_machine_id_in(dir.path()).unwrap();
        let token = dir.path().join("machine.token");
        fs::write(&token, "pair-token\n").unwrap();
        fs::remove_file(&token).unwrap();
        let after = load_or_create_machine_id_in(dir.path()).unwrap();
        assert_eq!(id, after);
        assert!(!token.exists());
    }

    #[test]
    fn stored_id_with_surrounding_whitespace_is_reused() {
        let dir = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::now_v7();
        fs::write(dir.path().join("machine.id"), format!(" \t{id}\n")).unwrap();
        let loaded = load_or_create_machine_id_in(dir.path()).unwrap();
        assert_eq!(loaded, id);
    }

    #[cfg(unix)]
    #[test]
    fn created_machine_id_file_is_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let _ = load_or_create_machine_id_in(dir.path()).unwrap();
        let path = dir.path().join("machine.id");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "machine.id must be owner-only, got {mode:o}");
    }

    #[test]
    fn machine_id_stays_on_config_path_after_install_dir_swap() {
        let config = tempfile::tempdir().unwrap();
        let install_old = tempfile::tempdir().unwrap();
        let install_new = tempfile::tempdir().unwrap();

        let id = load_or_create_machine_id_in(config.path()).unwrap();
        fs::write(install_old.path().join("fleet-host"), b"old-bin").unwrap();

        // Update swaps dest bins; identity must stay in the config dir.
        fs::write(install_new.path().join("fleet-host"), b"new-bin").unwrap();
        fs::write(
            install_new.path().join("machine.id"),
            "00000000-0000-0000-0000-000000000000\n",
        )
        .unwrap();

        let after = load_or_create_machine_id_in(config.path()).unwrap();
        assert_eq!(id, after);
        assert_eq!(
            fs::read_to_string(config.path().join("machine.id")).unwrap(),
            format!("{id}\n")
        );
        assert_eq!(
            machine_id_path(),
            config_dir().join("machine.id"),
            "reclaim path is the config-dir file, not next to the swapped host binary"
        );
        assert_ne!(
            install_new.path().join("machine.id"),
            config.path().join("machine.id")
        );
        assert!(!install_old.path().join("machine.id").exists());
    }

    /// catalog:unit-host-run-lock-exclusive — Dual fleet-host with the same
    /// host_id (docker vs kube on one box) must serialize on `run.lock`.
    /// Exclusive is flock, not "file exists": a leftover lock after crash
    /// must not brick the next start.
    #[cfg(unix)]
    #[test]
    fn acquire_run_lock_is_exclusive_for_same_host_id() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let same = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let host_id = uuid::Uuid::now_v7();
        // Same host_id + machine token in one config dir is the dual-host
        // docker-vs-kube case: both processes would reclaim the same row.
        fs::write(same.path().join("machine.id"), format!("{host_id}\n")).unwrap();
        fs::write(same.path().join("machine.token"), "flm_shared\n").unwrap();
        fs::write(
            same.path().join("config.toml"),
            format!("host_id = \"{host_id}\"\n"),
        )
        .unwrap();

        assert_eq!(
            run_lock_path(),
            config_dir().join("run.lock"),
            "public run.lock path is the config dir, next to machine.id"
        );
        assert_eq!(
            run_lock_path_in(same.path()),
            same.path().join("run.lock"),
            "run.lock lives in the config dir next to machine.id / machine.token"
        );

        // Stale run.lock from a previous process is not a holder.
        fs::write(same.path().join("run.lock"), b"stale-pid\n").unwrap();
        let first = acquire_run_lock_in(same.path()).expect("stale lock file is not exclusive");
        assert!(same.path().join("run.lock").is_file());

        // Same config dir = same host_id / machine token. A second process
        // (kube next to docker) must not also run.
        let err = acquire_run_lock_in(same.path()).expect_err("second holder same host_id");
        let msg = err.to_string();
        assert!(
            msg.contains("already running"),
            "exclusive run.lock must fail closed: {msg}"
        );
        assert!(
            msg.contains("run.lock"),
            "error must name the lock file so the operator can find the other host: {msg}"
        );

        // Two threads racing the same host still yield exactly one holder.
        let race_dir = tempfile::tempdir().unwrap();
        fs::write(race_dir.path().join("machine.id"), format!("{host_id}\n")).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let race_path = race_dir.path().to_path_buf();
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let race_path = race_path.clone();
                thread::spawn(move || {
                    barrier.wait();
                    acquire_run_lock_in(&race_path)
                })
            })
            .collect();
        let raced: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let wins = raced.iter().filter(|r| r.is_ok()).count();
        let losses = raced.iter().filter(|r| r.is_err()).count();
        assert_eq!(wins, 1, "exactly one racer must hold run.lock");
        assert_eq!(losses, 1, "the other racer must see already running");
        assert!(
            raced
                .iter()
                .filter_map(|r| r.as_ref().err())
                .any(|e| e.to_string().contains("already running")),
            "racy loser must fail closed, not a generic IO error: {raced:?}"
        );

        // A different box (different config dir / host_id) can run at the same time.
        let other_id = uuid::Uuid::now_v7();
        fs::write(other.path().join("machine.id"), format!("{other_id}\n")).unwrap();
        let _other_host =
            acquire_run_lock_in(other.path()).expect("different host_id must not share the lock");

        drop(first);
        acquire_run_lock_in(same.path()).expect("lock after drop");

        // Race winner dropping must let the other process start (crash/restart).
        let winner = raced.into_iter().find_map(|r| r.ok()).expect("one winner");
        drop(winner);
        acquire_run_lock_in(&race_path).expect("lock after race winner drop");
    }
}
