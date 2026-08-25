//! Paired worker host: pair with the control plane and run leased jobs.

pub mod agents;
pub mod bus;
pub mod chat_cli;
pub mod client;
pub mod config;
pub mod exec_k8s;
pub mod hardware;
pub mod paths;
pub mod platform;
pub mod run;
pub mod software;
pub mod spawn;
pub mod update;

pub use config::{ContainerBackend, LocalConfig, merge_child_env};
pub use paths::{config_dir, config_path, token_path};
pub use platform::{advertised_container, advertised_host, detect_platform, hostname};
pub use spawn::{kill_session, runtime_allowed};
