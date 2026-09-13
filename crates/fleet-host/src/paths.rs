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

pub fn last_heartbeat_path() -> PathBuf {
    config_dir().join("last_heartbeat.json")
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
/// 0600 and reused after token wipe / surrounding whitespace so
/// unpair+re-pair reclaims the same worker row.
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

    /// catalog:unit-host-run-lock-exclusive — one `fleet-host run` per
    /// config dir via flock (not file-exists). Leftover `run.lock` after
    /// crash is not a holder; a second dir can run at the same time.
    #[cfg(unix)]
    #[test]
    fn acquire_run_lock_stale_file_is_not_a_holder() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("run.lock"), b"stale-pid\n").unwrap();
        acquire_run_lock_in(dir.path()).expect("stale lock file is not exclusive");
    }

    #[cfg(unix)]
    #[test]
    fn acquire_run_lock_race_yields_exactly_one_holder() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let dir = tempfile::tempdir().unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let path = dir.path().to_path_buf();
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let path = path.clone();
                thread::spawn(move || {
                    barrier.wait();
                    acquire_run_lock_in(&path)
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
    }

    #[cfg(unix)]
    #[test]
    fn acquire_run_lock_other_dir_is_independent() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let _held = acquire_run_lock_in(a.path()).unwrap();
        acquire_run_lock_in(b.path()).expect("different config dir must not share the lock");
    }

    /// O_CLOEXEC: SIGKILL of the holder must not leave a spawned child's
    /// inherited fd holding flock.
    #[cfg(unix)]
    #[test]
    fn acquire_run_lock_cloexec_so_spawned_child_cannot_brick_restart() {
        use std::process::{Command, Stdio};

        let dir = tempfile::tempdir().unwrap();
        let lock_dir = dir.path().to_path_buf();
        let pid_path = lock_dir.join("sleep.pid");

        let acquirer = unsafe { libc::fork() };
        assert!(
            acquirer >= 0,
            "fork: {}",
            std::io::Error::last_os_error()
        );
        if acquirer == 0 {
            let lock = match acquire_run_lock_in(&lock_dir) {
                Ok(lock) => lock,
                Err(_) => unsafe { libc::_exit(1) },
            };
            let child = match Command::new("sleep")
                .arg("30")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(child) => child,
                Err(_) => unsafe { libc::_exit(2) },
            };
            let _ = fs::write(&pid_path, child.id().to_string());
            std::mem::forget(child);
            std::mem::forget(lock);
            unsafe { libc::_exit(0) };
        }

        let mut status = 0;
        let waited = unsafe { libc::waitpid(acquirer, &mut status, 0) };
        assert_eq!(
            waited,
            acquirer,
            "waitpid: {}",
            std::io::Error::last_os_error()
        );
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "acquirer _exit 0, status={status}"
        );

        let sleep_pid: libc::pid_t = fs::read_to_string(&pid_path)
            .expect("sleep pid")
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            unsafe { libc::kill(sleep_pid, 0) },
            0,
            "spawned child must still be alive"
        );
        let _restart = acquire_run_lock_in(&lock_dir)
            .expect("restart after acquirer _exit must succeed while spawned child lives");
        unsafe {
            libc::kill(sleep_pid, libc::SIGKILL);
            libc::waitpid(sleep_pid, std::ptr::null_mut(), 0);
        }
    }

    #[test]
    fn acquire_run_lock_creates_missing_config_dir() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing/nested");
        let _held = acquire_run_lock_in(&missing).expect("create_dir_all parent of run.lock");
        assert!(
            missing.join("run.lock").is_file(),
            "acquire must mkdir and create run.lock"
        );
    }
}
