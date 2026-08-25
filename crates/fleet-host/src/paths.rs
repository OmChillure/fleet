//! On-disk layout: `$HOME/.config/fleet-host/{config.toml,machine.id,machine.token}`.

use std::env;
use std::path::{Path, PathBuf};

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
}
