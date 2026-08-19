//! Paired worker host: pair with the control plane and run leased jobs.

pub mod client;
pub mod config;
pub mod paths;
pub mod platform;
pub mod run;
pub mod spawn;

pub use config::{ContainerBackend, LocalConfig, merge_child_env};
pub use paths::{config_dir, config_path, token_path};
pub use platform::{advertised_container, advertised_host, detect_platform, hostname};
