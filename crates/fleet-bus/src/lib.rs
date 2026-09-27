//! Thin Zenoh adapter: key-space + JSON codec for [`BusMessage`].
//!
//! Domain types stay in `agent-fleet-contracts`. This crate does not run
//! fleet-server subscribe or fleet-host publish loops.
//!
//! Live connect is opt-in via [`ZENOH_LOCATOR_ENV`] (`tcp/127.0.0.1:7447`).

#![forbid(unsafe_code)]

pub mod codec;
pub mod keys;
pub mod session;

pub use agent_fleet_contracts::BusMessage;
pub use codec::{decode_json, decode_json_str, encode_json, encode_json_string};
pub use keys::{
    DEFAULT_TENANT, KEY_EXPR_ALL_RUN_SHARE, KEY_EXPR_ALL_SANDBOX_EVENTS,
    KEY_EXPR_ALL_SANDBOX_FAILURES, KEY_EXPR_ALL_SESSION_EVENTS, TEAM_SHARE_KIND, event_type_chunk,
    key_for_liveliness_driver, key_for_liveliness_host, key_for_message, key_for_run_share,
    key_for_sandbox_cmd, key_for_sandbox_event, key_for_session_event,
};
pub use session::{
    ZENOH_LOCATOR_ENV, client_config, decode_sample, locator_from_env, open_session,
    put_bus_message, try_open_from_env,
};
