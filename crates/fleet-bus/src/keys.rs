//! Zenoh key expressions from `docs/architecture/transport-zenoh.md` §4.

use std::fmt;

/// Default tenant chunk used in the architecture examples.
pub const DEFAULT_TENANT: &str = "default";

/// `fleet/{tenant}/sandbox/{sandbox_id}/event/{type}`
///
/// `{type}` is the last path chunk (`ready`, `failed`, …). Callers that have a
/// catalogue name like `sandbox.failed` can pass [`event_type_chunk`].
pub fn key_for_sandbox_event(
    tenant: impl AsRef<str>,
    sandbox_id: impl fmt::Display,
    event_type: impl AsRef<str>,
) -> String {
    format!(
        "fleet/{}/sandbox/{}/event/{}",
        tenant.as_ref(),
        sandbox_id,
        event_type.as_ref()
    )
}

/// `fleet/{tenant}/sandbox/{sandbox_id}/cmd/{type}`
pub fn key_for_sandbox_cmd(
    tenant: impl AsRef<str>,
    sandbox_id: impl fmt::Display,
    cmd_type: impl AsRef<str>,
) -> String {
    format!(
        "fleet/{}/sandbox/{}/cmd/{}",
        tenant.as_ref(),
        sandbox_id,
        cmd_type.as_ref()
    )
}

/// `fleet/{tenant}/session/{session_id}/event/{type}`
pub fn key_for_session_event(
    tenant: impl AsRef<str>,
    session_id: impl fmt::Display,
    event_type: impl AsRef<str>,
) -> String {
    format!(
        "fleet/{}/session/{}/event/{}",
        tenant.as_ref(),
        session_id,
        event_type.as_ref()
    )
}

/// `fleet/{tenant}/liveliness/driver/{sandbox_id}`
pub fn key_for_liveliness_driver(tenant: impl AsRef<str>, sandbox_id: impl fmt::Display) -> String {
    format!("fleet/{}/liveliness/driver/{}", tenant.as_ref(), sandbox_id)
}

/// `fleet/{tenant}/liveliness/host/{host_id}`
pub fn key_for_liveliness_host(tenant: impl AsRef<str>, host_id: impl fmt::Display) -> String {
    format!("fleet/{}/liveliness/host/{}", tenant.as_ref(), host_id)
}

/// Wildcard: all sandbox failures fleet-wide.
pub const KEY_EXPR_ALL_SANDBOX_FAILURES: &str = "fleet/*/sandbox/*/event/failed";

/// All sandbox lifecycle events (API subscriber).
pub const KEY_EXPR_ALL_SANDBOX_EVENTS: &str = "fleet/*/sandbox/*/event/*";

/// All session facts (API subscriber).
pub const KEY_EXPR_ALL_SESSION_EVENTS: &str = "fleet/*/session/*/event/*";

/// Route a live [`BusMessage`] to its event key. Commands return `None`.
pub fn key_for_message(tenant: impl AsRef<str>, msg: &crate::BusMessage) -> Option<String> {
    let tenant = tenant.as_ref();
    let typ = event_type_chunk(msg.type_name());
    if msg.type_name().starts_with("session.") {
        let id = msg.envelope.session_id?;
        return Some(key_for_session_event(tenant, id, typ));
    }
    if msg.type_name().starts_with("sandbox.") {
        let id = msg.envelope.sandbox_id?;
        return Some(key_for_sandbox_event(tenant, id, typ));
    }
    None
}

/// Last `.`-separated chunk of a catalogue `type` (`sandbox.failed` → `failed`).
pub fn event_type_chunk(type_name: &str) -> &str {
    type_name
        .rsplit_once('.')
        .map(|(_, t)| t)
        .unwrap_or(type_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_event_matches_architecture_examples() {
        assert_eq!(
            key_for_sandbox_event("default", "9f3a", "ready"),
            "fleet/default/sandbox/9f3a/event/ready"
        );
        assert_eq!(
            key_for_sandbox_event(DEFAULT_TENANT, "9f3a", "failed"),
            "fleet/default/sandbox/9f3a/event/failed"
        );
    }

    #[test]
    fn liveliness_driver_and_host() {
        assert_eq!(
            key_for_liveliness_driver("acme", "sbx-1"),
            "fleet/acme/liveliness/driver/sbx-1"
        );
        assert_eq!(
            key_for_liveliness_host("acme", "host-42"),
            "fleet/acme/liveliness/host/host-42"
        );
    }

    #[test]
    fn event_type_chunk_strips_domain_prefix() {
        assert_eq!(event_type_chunk("sandbox.failed"), "failed");
        assert_eq!(event_type_chunk("session.state_changed"), "state_changed");
        assert_eq!(event_type_chunk("ready"), "ready");
    }

    #[test]
    fn key_for_message_session() {
        use agent_fleet_contracts::{
            BusMessage, MessageBody, Producer, SessionCompleted, SessionId,
        };
        let sid = SessionId::new();
        let msg = BusMessage::new(
            Producer::Other("fleet-host".into()),
            MessageBody::SessionCompleted(SessionCompleted {
                session_id: sid,
                summary: None,
            }),
        );
        assert_eq!(
            key_for_message("default", &msg).unwrap(),
            format!("fleet/default/session/{sid}/event/completed")
        );
    }

    fn session_and_cmd_keys() {
        assert_eq!(
            key_for_session_event("default", "sid", "state_changed"),
            "fleet/default/session/sid/event/state_changed"
        );
        assert_eq!(
            key_for_sandbox_cmd("default", "sbx", "stop"),
            "fleet/default/sandbox/sbx/cmd/stop"
        );
        assert_eq!(
            KEY_EXPR_ALL_SANDBOX_FAILURES,
            "fleet/*/sandbox/*/event/failed"
        );
    }
}
