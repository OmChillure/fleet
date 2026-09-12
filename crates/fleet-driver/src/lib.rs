//! Fleet driver: ACP client + orchestrator worker protocol.
//!
//! The `fleet-driver` binary is the guest of a paired `fleet-host`
//! (and of orchestrator tests that enable local spawn). Tests drive
//! [`acp::run_session`] against `mock-acp-agent` (no LLM).

/// Distinctive ASCII so fleet-host can read this file's package version.
/// Prefix is part of the contract — do not rename.
#[used]
static FLEET_DRIVER_PKG_STAMP: &[u8] =
    concat!("FLEET_DRIVER_PKG=", env!("FLEET_DRIVER_PKG_VERSION"), "\0").as_bytes();

pub fn pkg_version() -> &'static str {
    option_env!("FLEET_DRIVER_PKG_VERSION").unwrap_or("0.0.0")
}

pub mod acp;
pub mod cli_session;
pub mod config;
pub mod harness;
pub mod mock_ollama;
pub mod ollama_loop;
pub mod orch;
pub mod prompt;
pub mod provision;
pub mod report;
pub mod terminal;
