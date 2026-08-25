//! JSON encode/decode for [`agent_fleet_contracts::BusMessage`].
//!
//! Same bytes as the contracts crate (`to_json_vec` / `from_json_slice`): one
//! object with a flattened envelope plus adjacent `type` / `payload`. Zenoh
//! encoding for these payloads is `application/json`.

use agent_fleet_contracts::BusMessage;

/// Serialize a bus message to UTF-8 JSON bytes.
pub fn encode_json(msg: &BusMessage) -> Result<Vec<u8>, serde_json::Error> {
    msg.to_json_vec()
}

/// Serialize a bus message to a UTF-8 JSON string.
pub fn encode_json_string(msg: &BusMessage) -> Result<String, serde_json::Error> {
    msg.to_json()
}

/// Parse a bus message from UTF-8 JSON bytes.
pub fn decode_json(bytes: &[u8]) -> Result<BusMessage, serde_json::Error> {
    BusMessage::from_json_slice(bytes)
}

/// Parse a bus message from a UTF-8 JSON string.
pub fn decode_json_str(s: &str) -> Result<BusMessage, serde_json::Error> {
    BusMessage::from_json(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_fleet_contracts::{
        BusMessage, MessageBody, Producer, SCHEMA_VERSION, SandboxFailed, SandboxFailureReason,
        SandboxFailureSeverity, SandboxId,
    };

    fn failed_msg(sandbox_id: SandboxId) -> BusMessage {
        let mut msg = BusMessage::new(
            Producer::SandboxManager,
            MessageBody::SandboxFailed(SandboxFailed {
                sandbox_id,
                severity: SandboxFailureSeverity::Failed,
                reason_code: SandboxFailureReason::OomKilled,
                message: "Pod OOMKilled by kubelet".into(),
                restart_count: Some(1),
                backend_raw: None,
            }),
        );
        msg.envelope.tenant_id = Some("default".into());
        msg
    }

    #[test]
    fn sandbox_failed_json_roundtrip() {
        let sandbox_id = SandboxId::new();
        let msg = failed_msg(sandbox_id);
        let bytes = encode_json(&msg).unwrap();
        let back = decode_json(&bytes).unwrap();
        assert_eq!(back, msg);
        assert_eq!(back.type_name(), "sandbox.failed");
        assert_eq!(back.envelope.sandbox_id, Some(sandbox_id));
    }

    #[test]
    fn string_roundtrip_matches_bytes() {
        let msg = failed_msg(SandboxId::new());
        let s = encode_json_string(&msg).unwrap();
        assert_eq!(s.as_bytes(), encode_json(&msg).unwrap());
        assert_eq!(decode_json_str(&s).unwrap(), msg);
    }

    #[test]
    fn wire_shape_has_type_and_payload() {
        let msg = failed_msg(SandboxId::new());
        let value: serde_json::Value = serde_json::from_slice(&encode_json(&msg).unwrap()).unwrap();
        assert_eq!(value["type"], "sandbox.failed");
        assert_eq!(value["producer"], "sandbox-manager");
        assert_eq!(value["schema_version"], SCHEMA_VERSION);
        assert_eq!(value["tenant_id"], "default");
        assert_eq!(value["payload"]["reason_code"], "oom_killed");
        assert!(value.get("payload").is_some());
    }

    #[test]
    fn cmd_ack_roundtrip() {
        let msg = BusMessage::new(
            Producer::Orchestrator,
            MessageBody::CmdAck(agent_fleet_contracts::CmdAck {
                accepted: true,
                error: None,
            }),
        );
        let back = decode_json(&encode_json(&msg).unwrap()).unwrap();
        assert_eq!(back.type_name(), "cmd.ack");
        assert_eq!(back, msg);
    }
}
