//! Fleet driver: ACP client + orchestrator worker protocol.
//!
//! The `fleet-driver` binary is a drop-in `PROCESS_WORKER_BIN` for
//! `SANDBOX_BACKEND=process`. Tests drive [`acp::run_session`] against
//! `mock-acp-agent` (no LLM).

pub mod acp;
pub mod config;
pub mod harness;
pub mod orch;
pub mod prompt;
pub mod provision;
pub mod report;
pub mod terminal;
