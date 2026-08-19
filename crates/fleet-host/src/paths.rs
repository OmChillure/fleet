//! On-disk layout: `$HOME/.config/fleet-host/{config.toml,machine.token}`.

use std::env;
use std::path::PathBuf;

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
