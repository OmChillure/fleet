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
