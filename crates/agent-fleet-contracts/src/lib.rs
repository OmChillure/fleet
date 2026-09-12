//! Shared command/event contracts for the Agent Fleet control plane.
//!
//! Domain catalogue: `docs/architecture/events.md`.
//!
//! Default wire format is JSON via `serde` / `serde_json`. Transport (Zenoh,
//! NATS, Postgres JSONB, …) is out of scope — this crate only owns the schema.
//!
//! # Example
//!
//! ```
//! use agent_fleet_contracts::{
//!     BusMessage, MessageBody, Producer, SandboxFailed, SandboxFailureReason,
//!     SandboxFailureSeverity, SandboxId,
//! };
//!
//! let sandbox_id = SandboxId::new();
//! let msg = BusMessage::new(
//!     Producer::SandboxManager,
//!     MessageBody::SandboxFailed(SandboxFailed {
//!         sandbox_id,
//!         severity: SandboxFailureSeverity::Failed,
//!         reason_code: SandboxFailureReason::OomKilled,
//!         message: "Pod OOMKilled by kubelet".into(),
//!         restart_count: Some(1),
//!         backend_raw: None,
//!     }),
//! );
//! assert_eq!(msg.envelope.sandbox_id, Some(sandbox_id));
//!
//! let json = msg.to_json().unwrap();
//! let back = BusMessage::from_json(&json).unwrap();
//! assert_eq!(back.type_name(), "sandbox.failed");
//! ```

#![forbid(unsafe_code)]

mod agent;
mod driver;
mod envelope;
mod guest_files;
mod ids;
mod integration;
mod isolation;
mod message;
mod sandbox;
mod serde_duration;
mod session;
mod types;

pub use agent::*;
pub use driver::*;
pub use envelope::*;
pub use guest_files::*;
pub use ids::*;
pub use integration::*;
pub use isolation::*;
pub use message::*;
pub use sandbox::*;
pub use session::*;
pub use types::*;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn sandbox_failed_roundtrip_matches_catalogue_shape() {
        let sandbox_id = SandboxId::from_uuid(uuid::Uuid::nil());
        let session_id = SessionId::from_uuid(uuid::Uuid::from_u128(1));
        let message_id = MessageId::from_uuid(uuid::Uuid::from_u128(2));
        let causation_id = MessageId::from_uuid(uuid::Uuid::from_u128(3));

        let envelope = Envelope {
            message_id,
            ts: Utc.with_ymd_and_hms(2026, 7, 28, 12, 34, 56).unwrap(),
            producer: Producer::SandboxManager,
            schema_version: SCHEMA_VERSION.to_string(),
            session_id: Some(session_id),
            sandbox_id: Some(sandbox_id),
            task_id: None,
            workflow_id: None,
            turn_id: None,
            correlation_id: MessageId::from_uuid(session_id.as_uuid()),
            causation_id: Some(causation_id),
            tenant_id: Some("default".into()),
        };

        let msg = BusMessage::with_envelope(
            envelope,
            MessageBody::SandboxFailed(SandboxFailed {
                sandbox_id,
                severity: SandboxFailureSeverity::Failed,
                reason_code: SandboxFailureReason::OomKilled,
                message: "Pod OOMKilled by kubelet".into(),
                restart_count: Some(1),
                backend_raw: None,
            }),
        );

        let value = serde_json::to_value(&msg).unwrap();
        assert_eq!(value["type"], "sandbox.failed");
        assert_eq!(value["producer"], "sandbox-manager");
        assert_eq!(value["schema_version"], SCHEMA_VERSION);
        assert_eq!(
            value["payload"]["sandbox_id"],
            uuid::Uuid::nil().to_string()
        );
        assert_eq!(value["payload"]["reason_code"], "oom_killed");
        assert_eq!(value["payload"]["severity"], "failed");
        assert_eq!(value["payload"]["restart_count"], 1);
        assert!(value.get("task_id").is_none());

        let back: BusMessage = serde_json::from_value(value).unwrap();
        assert_eq!(back.type_name(), "sandbox.failed");
        assert_eq!(back, msg);
    }

    #[test]
    fn bus_message_new_merges_sandbox_scope() {
        let sandbox_id = SandboxId::new();
        let msg = BusMessage::new(
            Producer::SandboxManager,
            MessageBody::SandboxFailed(SandboxFailed {
                sandbox_id,
                severity: SandboxFailureSeverity::Failed,
                reason_code: SandboxFailureReason::OomKilled,
                message: "oom".into(),
                restart_count: None,
                backend_raw: None,
            }),
        );
        assert_eq!(msg.envelope.sandbox_id, Some(sandbox_id));
        assert_eq!(msg.envelope.session_id, None);
    }

    #[test]
    fn bus_message_new_merges_session_and_turn() {
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let msg = BusMessage::new(
            Producer::Driver,
            MessageBody::AgentEvent(AgentEvent {
                session_id,
                turn_id,
                seq: 1,
                kind: "thought".into(),
                acp_method: None,
                payload: None,
                redaction: Redaction::None,
            }),
        );
        assert_eq!(msg.envelope.session_id, Some(session_id));
        assert_eq!(msg.envelope.turn_id, Some(turn_id));
    }

    #[test]
    fn agent_event_stream_chunk() {
        let session_id = SessionId::from_uuid(uuid::Uuid::from_u128(1));
        let turn_id = TurnId::from_uuid(uuid::Uuid::from_u128(2));
        let message_id = MessageId::from_uuid(uuid::Uuid::from_u128(3));
        let causation_id = MessageId::from_uuid(uuid::Uuid::from_u128(4));

        let envelope = Envelope {
            message_id,
            ts: Utc.with_ymd_and_hms(2026, 7, 28, 12, 34, 56).unwrap(),
            producer: Producer::Driver,
            schema_version: SCHEMA_VERSION.to_string(),
            session_id: Some(session_id),
            sandbox_id: None,
            task_id: None,
            workflow_id: None,
            turn_id: Some(turn_id),
            correlation_id: MessageId::from_uuid(session_id.as_uuid()),
            causation_id: Some(causation_id),
            tenant_id: Some("default".into()),
        };

        let msg = BusMessage::with_envelope(
            envelope,
            MessageBody::AgentEvent(AgentEvent {
                session_id,
                turn_id,
                seq: 42,
                kind: "message_delta".into(),
                acp_method: None,
                payload: Some(serde_json::json!({"text": "hello"})),
                redaction: Redaction::None,
            }),
        );

        let value = serde_json::to_value(&msg).unwrap();
        assert_eq!(value["type"], "agent.event");
        assert_eq!(value["producer"], "driver");
        assert_eq!(value["schema_version"], SCHEMA_VERSION);
        assert_eq!(value["payload"]["seq"], 42);
        assert_eq!(value["payload"]["kind"], "message_delta");
        assert_eq!(value["payload"]["payload"]["text"], "hello");
        assert_eq!(value["payload"]["redaction"], "none");
        assert_eq!(
            value["payload"]["session_id"],
            uuid::Uuid::from_u128(1).to_string()
        );
        assert_eq!(
            value["payload"]["turn_id"],
            uuid::Uuid::from_u128(2).to_string()
        );
        assert!(value["payload"].get("acp_method").is_none());

        let back: BusMessage = serde_json::from_value(value).unwrap();
        assert_eq!(back.type_name(), "agent.event");
        assert_eq!(back, msg);
    }

    #[test]
    fn agent_event_deserializes_catalogue_json_with_omitted_acp_method() {
        let inbound = serde_json::json!({
            "message_id": "00000000-0000-0000-0000-000000000003",
            "ts": "2026-07-28T12:34:56Z",
            "producer": "driver",
            "schema_version": SCHEMA_VERSION,
            "session_id": "00000000-0000-0000-0000-000000000001",
            "turn_id": "00000000-0000-0000-0000-000000000002",
            "correlation_id": "00000000-0000-0000-0000-000000000001",
            "type": "agent.event",
            "payload": {
                "session_id": "00000000-0000-0000-0000-000000000001",
                "turn_id": "00000000-0000-0000-0000-000000000002",
                "seq": 42,
                "kind": "message_delta",
                "payload": {"text": "hello"},
                "redaction": "none"
            }
        });

        let msg: BusMessage = serde_json::from_value(inbound).unwrap();
        assert_eq!(msg.type_name(), "agent.event");
        match msg.body {
            MessageBody::AgentEvent(e) => {
                assert_eq!(e.session_id, SessionId::from_uuid(uuid::Uuid::from_u128(1)));
                assert_eq!(e.turn_id, TurnId::from_uuid(uuid::Uuid::from_u128(2)));
                assert_eq!(e.seq, 42);
                assert_eq!(e.kind, "message_delta");
                assert_eq!(e.acp_method, None);
                assert_eq!(e.payload, Some(serde_json::json!({"text": "hello"})));
                assert_eq!(e.redaction, Redaction::None);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn agent_event_redaction_wires_as_snake_case() {
        for (level, wire) in [
            (Redaction::None, "none"),
            (Redaction::Partial, "partial"),
            (Redaction::Full, "full"),
        ] {
            let msg = BusMessage::new(
                Producer::Driver,
                MessageBody::AgentEvent(AgentEvent {
                    session_id: SessionId::from_uuid(uuid::Uuid::from_u128(1)),
                    turn_id: TurnId::from_uuid(uuid::Uuid::from_u128(2)),
                    seq: 1,
                    kind: "thought".into(),
                    acp_method: None,
                    payload: None,
                    redaction: level,
                }),
            );
            let value = serde_json::to_value(&msg).unwrap();
            assert_eq!(value["type"], "agent.event");
            assert_eq!(value["payload"]["redaction"], wire);
            assert!(value["payload"].get("payload").is_none());
            let back: BusMessage = serde_json::from_value(value).unwrap();
            match back.body {
                MessageBody::AgentEvent(e) => assert_eq!(e.redaction, level),
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    #[test]
    fn integration_agent_message_stream_chunk_preserves_text_and_is_final() {
        let session_id = SessionId::from_uuid(uuid::Uuid::from_u128(1));
        let turn_id = TurnId::from_uuid(uuid::Uuid::from_u128(2));
        let msg = BusMessage::new(
            Producer::Orchestrator,
            MessageBody::IntegrationAgentMessage(AgentMessage {
                session_id,
                turn_id,
                channel: "slack".into(),
                thread_ref: "C1/thread".into(),
                text: "hello".into(),
                is_final: true,
                markdown: None,
            }),
        );

        let value = serde_json::to_value(&msg).unwrap();
        assert_eq!(value["type"], "integration.agent_message");
        assert_eq!(value["payload"]["text"], "hello");
        assert_eq!(value["payload"]["is_final"], true);
        assert_eq!(
            value["payload"]["session_id"],
            uuid::Uuid::from_u128(1).to_string()
        );
        assert_eq!(
            value["payload"]["turn_id"],
            uuid::Uuid::from_u128(2).to_string()
        );
        assert!(value["payload"].get("markdown").is_none());

        let back: BusMessage = serde_json::from_value(value).unwrap();
        assert_eq!(back.type_name(), "integration.agent_message");
        match back.body {
            MessageBody::IntegrationAgentMessage(m) => {
                assert_eq!(m.text, "hello");
                assert!(m.is_final);
                assert_eq!(m.markdown, None);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn cmd_ack_roundtrip() {
        let message_id = MessageId::from_uuid(uuid::Uuid::from_u128(2));
        let envelope = Envelope {
            message_id,
            ts: Utc.with_ymd_and_hms(2026, 7, 28, 12, 34, 56).unwrap(),
            producer: Producer::SandboxManager,
            schema_version: SCHEMA_VERSION.to_string(),
            session_id: None,
            sandbox_id: None,
            task_id: None,
            workflow_id: None,
            turn_id: None,
            correlation_id: message_id,
            causation_id: None,
            tenant_id: None,
        };

        let msg = BusMessage::with_envelope(
            envelope,
            MessageBody::CmdAck(CmdAck {
                accepted: true,
                error: None,
            }),
        );

        let value = serde_json::to_value(&msg).unwrap();
        assert_eq!(value["type"], "cmd.ack");
        assert_eq!(value["producer"], "sandbox-manager");
        assert_eq!(value["schema_version"], SCHEMA_VERSION);
        assert_eq!(value["payload"]["accepted"], true);
        assert!(value["payload"].get("error").is_none());

        let back: BusMessage = serde_json::from_value(value).unwrap();
        assert_eq!(back.type_name(), "cmd.ack");
        assert_eq!(back, msg);
        match &back.body {
            MessageBody::CmdAck(ack) => {
                assert!(ack.accepted);
                assert_eq!(ack.error, None);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn cmd_ack_wire_type_and_payload_accepted() {
        let msg = BusMessage::new(
            Producer::SandboxManager,
            MessageBody::CmdAck(CmdAck {
                accepted: true,
                error: None,
            }),
        );
        let value = serde_json::to_value(&msg).unwrap();
        assert_eq!(value["type"], "cmd.ack");
        assert_eq!(value["payload"]["accepted"], true);
        assert!(value["payload"].get("error").is_none());
    }

    #[test]
    fn cmd_ack_rejected_roundtrip_preserves_error() {
        let msg = BusMessage::new(
            Producer::SandboxManager,
            MessageBody::CmdAck(CmdAck {
                accepted: false,
                error: Some("sandbox already exists".into()),
            }),
        );
        let value = serde_json::to_value(&msg).unwrap();
        assert_eq!(value["type"], "cmd.ack");
        assert_eq!(value["payload"]["accepted"], false);
        assert_eq!(value["payload"]["error"], "sandbox already exists");

        let back: BusMessage = serde_json::from_value(value).unwrap();
        match back.body {
            MessageBody::CmdAck(ack) => {
                assert!(!ack.accepted);
                assert_eq!(ack.error.as_deref(), Some("sandbox already exists"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn cmd_ack_deserializes_catalogue_queryable_json() {
        let inbound = serde_json::json!({
            "message_id": "00000000-0000-0000-0000-000000000002",
            "ts": "2026-07-28T12:34:56Z",
            "producer": "sandbox-manager",
            "schema_version": SCHEMA_VERSION,
            "correlation_id": "00000000-0000-0000-0000-000000000002",
            "type": "cmd.ack",
            "payload": {
                "accepted": true
            }
        });

        let msg: BusMessage = serde_json::from_value(inbound).unwrap();
        assert_eq!(msg.type_name(), "cmd.ack");
        match msg.body {
            MessageBody::CmdAck(ack) => {
                assert!(ack.accepted);
                assert_eq!(ack.error, None);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn producer_integration_prefix() {
        let p = Producer::Integration("slack".into());
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(s, "\"integration:slack\"");
        let back: Producer = serde_json::from_str(&s).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn producer_unknown_label_preserved() {
        let p: Producer = serde_json::from_str("\"orchestrator-v2\"").unwrap();
        assert_eq!(p, Producer::Other("orchestrator-v2".into()));
        assert_eq!(serde_json::to_string(&p).unwrap(), "\"orchestrator-v2\"");
    }

    #[test]
    fn sandbox_backend_unknown_roundtrip() {
        let b: SandboxBackend = serde_json::from_str("\"firecracker\"").unwrap();
        assert_eq!(b, SandboxBackend::Other("firecracker".into()));
        assert_eq!(serde_json::to_string(&b).unwrap(), "\"firecracker\"");

        let known: SandboxBackend = serde_json::from_str("\"k8s\"").unwrap();
        assert_eq!(known, SandboxBackend::K8s);
        assert_eq!(serde_json::to_string(&known).unwrap(), "\"k8s\"");
    }

    #[test]
    fn sandbox_create_roundtrip_matches_catalogue_shape() {
        let sandbox_id = SandboxId::from_uuid(uuid::Uuid::nil());
        let session_id = SessionId::from_uuid(uuid::Uuid::from_u128(1));
        let message_id = MessageId::from_uuid(uuid::Uuid::from_u128(2));
        let causation_id = MessageId::from_uuid(uuid::Uuid::from_u128(3));

        let envelope = Envelope {
            message_id,
            ts: Utc.with_ymd_and_hms(2026, 7, 28, 12, 34, 56).unwrap(),
            producer: Producer::Orchestrator,
            schema_version: SCHEMA_VERSION.to_string(),
            session_id: Some(session_id),
            sandbox_id: Some(sandbox_id),
            task_id: None,
            workflow_id: None,
            turn_id: None,
            correlation_id: MessageId::from_uuid(session_id.as_uuid()),
            causation_id: Some(causation_id),
            tenant_id: Some("default".into()),
        };

        let msg = BusMessage::with_envelope(
            envelope,
            MessageBody::SandboxCreate(CreateSandbox {
                sandbox_id,
                session_ids: vec![session_id],
                backend: SandboxBackend::K8s,
                image: "ghcr.io/example/driver:latest".into(),
                resources: ResourceSpec {
                    cpu: Some("2".into()),
                    memory: Some("8Gi".into()),
                    disk: Some("40Gi".into()),
                    gpu: None,
                },
                network_policy: NetworkPolicy {
                    egress_mode: EgressMode::Allowlist,
                    allowlist: vec!["github.com".into()],
                },
                driver: DriverLaunchSpec {
                    image_or_binary: "driver".into(),
                    config_blob_ref: "blob://cfg/1".into(),
                    workspace_spec_ref: "blob://ws/1".into(),
                },
                labels: Default::default(),
                ttl: Some(std::time::Duration::from_secs(3600)),
                idle_timeout: None,
            }),
        );

        let value = serde_json::to_value(&msg).unwrap();
        assert_eq!(value["type"], "sandbox.create");
        assert_eq!(value["producer"], "orchestrator");
        assert_eq!(value["schema_version"], SCHEMA_VERSION);
        assert_eq!(
            value["payload"]["sandbox_id"],
            uuid::Uuid::nil().to_string()
        );
        assert_eq!(
            value["payload"]["session_ids"],
            serde_json::json!([uuid::Uuid::from_u128(1).to_string()])
        );
        assert_eq!(value["payload"]["backend"], "k8s");
        assert_eq!(value["payload"]["image"], "ghcr.io/example/driver:latest");
        assert_eq!(value["payload"]["resources"]["cpu"], "2");
        assert_eq!(value["payload"]["resources"]["memory"], "8Gi");
        assert_eq!(value["payload"]["resources"]["disk"], "40Gi");
        assert!(value["payload"]["resources"].get("gpu").is_none());
        assert_eq!(
            value["payload"]["network_policy"]["egress_mode"],
            "allowlist"
        );
        assert_eq!(
            value["payload"]["network_policy"]["allowlist"],
            serde_json::json!(["github.com"])
        );
        assert_eq!(value["payload"]["driver"]["image_or_binary"], "driver");
        assert_eq!(value["payload"]["driver"]["config_blob_ref"], "blob://cfg/1");
        assert_eq!(
            value["payload"]["driver"]["workspace_spec_ref"],
            "blob://ws/1"
        );
        assert_eq!(value["payload"]["ttl"], 3600);
        assert!(value["payload"].get("idle_timeout").is_none());
        assert!(value["payload"].get("labels").is_none());

        let back: BusMessage = serde_json::from_value(value).unwrap();
        assert_eq!(back.type_name(), "sandbox.create");
        assert_eq!(back, msg);
    }

    #[test]
    fn bus_message_new_mirrors_first_create_sandbox_session_id() {
        let sandbox_id = SandboxId::from_uuid(uuid::Uuid::from_u128(10));
        let first = SessionId::from_uuid(uuid::Uuid::from_u128(1));
        let second = SessionId::from_uuid(uuid::Uuid::from_u128(2));
        let body = |session_ids: Vec<SessionId>| {
            MessageBody::SandboxCreate(CreateSandbox {
                sandbox_id,
                session_ids,
                backend: SandboxBackend::K8s,
                image: "ghcr.io/example/driver:latest".into(),
                resources: ResourceSpec::default(),
                network_policy: NetworkPolicy {
                    egress_mode: EgressMode::None,
                    allowlist: vec![],
                },
                driver: DriverLaunchSpec {
                    image_or_binary: "driver".into(),
                    config_blob_ref: "blob://cfg/1".into(),
                    workspace_spec_ref: "blob://ws/1".into(),
                },
                labels: Default::default(),
                ttl: None,
                idle_timeout: None,
            })
        };

        let two = BusMessage::new(Producer::Orchestrator, body(vec![first, second]));
        assert_eq!(two.envelope.sandbox_id, Some(sandbox_id));
        assert_eq!(two.envelope.session_id, Some(first));

        let empty = BusMessage::new(Producer::Orchestrator, body(vec![]));
        assert_eq!(empty.envelope.sandbox_id, Some(sandbox_id));
        assert_eq!(empty.envelope.session_id, None);
    }

    #[test]
    fn sandbox_create_deserializes_catalogue_json_with_omitted_optionals() {
        let inbound = serde_json::json!({
            "message_id": "00000000-0000-0000-0000-000000000002",
            "ts": "2026-07-28T12:34:56Z",
            "producer": "orchestrator",
            "schema_version": SCHEMA_VERSION,
            "correlation_id": "00000000-0000-0000-0000-000000000002",
            "type": "sandbox.create",
            "payload": {
                "sandbox_id": "00000000-0000-0000-0000-000000000000",
                "backend": "k8s",
                "image": "ghcr.io/example/driver:latest",
                "resources": {
                    "cpu": "2",
                    "memory": "8Gi",
                    "disk": "40Gi"
                },
                "network_policy": {
                    "egress_mode": "allowlist",
                    "allowlist": ["github.com"]
                },
                "driver": {
                    "image_or_binary": "driver",
                    "config_blob_ref": "blob://cfg/1",
                    "workspace_spec_ref": "blob://ws/1"
                }
            }
        });

        let msg: BusMessage = serde_json::from_value(inbound).unwrap();
        assert_eq!(msg.type_name(), "sandbox.create");
        match msg.body {
            MessageBody::SandboxCreate(c) => {
                assert_eq!(c.sandbox_id, SandboxId::from_uuid(uuid::Uuid::nil()));
                assert!(c.session_ids.is_empty());
                assert!(c.labels.is_empty());
                assert_eq!(c.ttl, None);
                assert_eq!(c.idle_timeout, None);
                assert_eq!(c.backend, SandboxBackend::K8s);
                assert_eq!(c.image, "ghcr.io/example/driver:latest");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
