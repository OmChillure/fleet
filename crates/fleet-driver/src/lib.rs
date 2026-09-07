//! Fleet driver: guest of a paired `fleet-host`.
//!
//! Real models (Grok / Claude / Ollama) use the official CLI on that box —
//! same family as chat `chat_turn`. `mock` still speaks ACP for tests.
//! The control plane never runs the agent.

/// Distinctive ASCII so fleet-host can read this file's package version.
/// Prefix is part of the contract — do not rename.
#[used]
static FLEET_DRIVER_PKG_STAMP: &[u8] =
    concat!("FLEET_DRIVER_PKG=", env!("FLEET_DRIVER_PKG_VERSION"), "\0").as_bytes();

pub fn pkg_version() -> &'static str {
    option_env!("FLEET_DRIVER_PKG_VERSION").unwrap_or("0.0.0")
}

pub mod acp;
pub mod config;
pub mod harness;
pub mod official_cli;
pub mod orch;
pub mod prompt;
pub mod provision;
pub mod report;
pub mod terminal;
