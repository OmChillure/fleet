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
        let msg = BusMessage::new(
            Producer::Driver,
            MessageBody::AgentEvent(AgentEvent {
                session_id: SessionId::new(),
                turn_id: TurnId::new(),
                seq: 42,
                kind: "message_delta".into(),
                acp_method: None,
                payload: Some(serde_json::json!({"text": "hello"})),
                redaction: Redaction::None,
            }),
        );
        let json = msg.to_json().unwrap();
        let back = BusMessage::from_json(&json).unwrap();
        match back.body {
            MessageBody::AgentEvent(e) => {
                assert_eq!(e.seq, 42);
                assert_eq!(e.kind, "message_delta");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn cmd_ack_roundtrip() {
        let msg = BusMessage::new(
            Producer::SandboxManager,
            MessageBody::CmdAck(CmdAck {
                accepted: true,
                error: None,
            }),
        );
        let back = BusMessage::from_json(&msg.to_json().unwrap()).unwrap();
        assert_eq!(back.type_name(), "cmd.ack");
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
    fn create_sandbox_command_json() {
        let sandbox_id = SandboxId::new();
        let session_id = SessionId::new();
        let msg = BusMessage::new(
            Producer::Orchestrator,
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
        assert_eq!(msg.envelope.sandbox_id, Some(sandbox_id));
        assert_eq!(msg.envelope.session_id, Some(session_id));
        let v = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "sandbox.create");
        assert_eq!(v["payload"]["backend"], "k8s");
        assert_eq!(v["payload"]["ttl"], 3600);
        let _back: BusMessage = serde_json::from_value(v).unwrap();
    }
}
