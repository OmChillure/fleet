//! Zenoh key expressions from `docs/architecture/transport-zenoh.md` §4.
//!
//! Live HTTP worker status is `WorkerSessionStatus` (`spawning` / `healthy` / …).
//! Catalogue `SessionState` is bus payload only (`session.state_changed`).

use std::fmt;

/// Default tenant chunk used in the architecture examples.
pub const DEFAULT_TENANT: &str = "default";

/// `agent.event` kind for a run-scoped team share fact (not a session trace).
pub const TEAM_SHARE_KIND: &str = "team_share";

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
///
/// `session.state_changed` carries catalogue `SessionState`. That is not the
/// live HTTP field: board/tick status is `WorkerSessionStatus`.
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

/// `fleet/{workspace}/run/{run_id}/share/{aspect}`
///
/// Mid-run team facts (chat, asks). `{aspect}` may contain `/` (e.g. `chat/{id}`).
pub fn key_for_run_share(
    workspace: impl AsRef<str>,
    run_id: impl fmt::Display,
    aspect: impl AsRef<str>,
) -> String {
    format!(
        "fleet/{}/run/{}/share/{}",
        workspace.as_ref(),
        run_id,
        aspect.as_ref()
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

/// All run-scoped mid-run share facts (team chat and other guest-visible posts).
pub const KEY_EXPR_ALL_RUN_SHARE: &str = "fleet/*/run/*/share/**";

/// Route a live [`BusMessage`] to its event key. Commands return `None`.
pub fn key_for_message(tenant: impl AsRef<str>, msg: &crate::BusMessage) -> Option<String> {
    let tenant = tenant.as_ref();
    if let agent_fleet_contracts::MessageBody::AgentEvent(ev) = &msg.body {
        if ev.kind == TEAM_SHARE_KIND {
            let workspace = msg
                .envelope
                .tenant_id
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(tenant);
            let run = msg
                .envelope
                .workflow_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| ev.session_id.to_string());
            return Some(key_for_run_share(
                workspace,
                run,
                format!("chat/{}", ev.turn_id),
            ));
        }
    }
    let typ = event_type_chunk(msg.type_name());
    if msg.type_name().starts_with("session.") {
        let id = msg.envelope.session_id?;
        return Some(key_for_session_event(tenant, id, typ));
    }
    if msg.type_name().starts_with("sandbox.") {
        let id = msg.envelope.sandbox_id?;
        return Some(key_for_sandbox_event(tenant, id, typ));
    }
    if msg.type_name() == "agent.event" {
        let id = msg.envelope.session_id?;
        return Some(key_for_session_event(tenant, id, "agent"));
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

    #[test]
    fn key_for_message_agent_event() {
        use agent_fleet_contracts::{
            AgentEvent, BusMessage, MessageBody, Producer, Redaction, SessionId, TurnId,
        };
        let sid = SessionId::new();
        let msg = BusMessage::new(
            Producer::Driver,
            MessageBody::AgentEvent(AgentEvent {
                session_id: sid,
                turn_id: TurnId::new(),
                seq: 1,
                kind: "batch".into(),
                acp_method: None,
                payload: None,
                redaction: Redaction::None,
            }),
        );
        assert_eq!(
            key_for_message("default", &msg).unwrap(),
            format!("fleet/default/session/{sid}/event/agent")
        );
    }

    #[test]
    fn run_share_key_matches_architecture() {
        let ws = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
        let run = "11111111-1111-4111-8111-111111111111";
        let chat = "22222222-2222-4222-8222-222222222222";
        assert_eq!(
            key_for_run_share(ws, run, format!("chat/{chat}")),
            format!("fleet/{ws}/run/{run}/share/chat/{chat}")
        );
        assert_eq!(KEY_EXPR_ALL_RUN_SHARE, "fleet/*/run/*/share/**");
    }

    #[test]
    fn run_share_key_and_codec_roundtrip() {
        use crate::{decode_json, encode_json};
        use agent_fleet_contracts::{
            AgentEvent, BusMessage, MessageBody, Producer, Redaction, SessionId, TurnId, WorkflowId,
        };

        let workspace = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
        let run = SessionId::new();
        let chat_id = TurnId::new();
        let mut msg = BusMessage::new(
            Producer::Orchestrator,
            MessageBody::AgentEvent(AgentEvent {
                session_id: run,
                turn_id: chat_id,
                seq: 1,
                kind: TEAM_SHARE_KIND.into(),
                acp_method: None,
                payload: Some(serde_json::json!({ "body": "API is POST /hello" })),
                redaction: Redaction::None,
            }),
        );
        msg.envelope.tenant_id = Some(workspace.into());
        msg.envelope.workflow_id = Some(WorkflowId::from_uuid(run.as_uuid()));

        let bytes = encode_json(&msg).expect("encode");
        let back = decode_json(&bytes).expect("decode");
        assert_eq!(back.type_name(), "agent.event");
        match &back.body {
            MessageBody::AgentEvent(ev) => assert_eq!(ev.kind, TEAM_SHARE_KIND),
            other => panic!("expected agent.event, got {other:?}"),
        }
        assert_eq!(
            key_for_message(workspace, &back).unwrap(),
            format!("fleet/{workspace}/run/{run}/share/chat/{chat_id}")
        );
    }
}
