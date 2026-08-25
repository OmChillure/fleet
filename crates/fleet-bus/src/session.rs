//! Optional live Zenoh session. Connect only when `ZENOH_LOCATOR` is set.

use agent_fleet_contracts::BusMessage;
use zenoh::bytes::Encoding;
use zenoh::qos::CongestionControl;
use zenoh::{Config, Session};

use crate::codec::encode_json;

/// Locator for a `zenohd` (or peer) endpoint, e.g. `tcp/127.0.0.1:7447`.
pub const ZENOH_LOCATOR_ENV: &str = "ZENOH_LOCATOR";

/// Read [`ZENOH_LOCATOR_ENV`]. Empty / unset → `None` (unit tests stay offline).
pub fn locator_from_env() -> Option<String> {
    match std::env::var(ZENOH_LOCATOR_ENV) {
        Ok(s) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        }
        Err(_) => None,
    }
}

/// Client-mode config that connects to a single locator (no multicast scout).
pub fn client_config(locator: &str) -> zenoh::Result<Config> {
    let loc = serde_json::to_string(locator).expect("locator is a string");
    let json5 = format!(
        r#"{{
            mode: "client",
            connect: {{ endpoints: [{loc}] }},
            scouting: {{ multicast: {{ enabled: false }} }}
        }}"#
    );
    Config::from_json5(&json5)
}

/// Open a client session to `locator`.
pub async fn open_session(locator: &str) -> zenoh::Result<Session> {
    zenoh::open(client_config(locator)?).await
}

/// Open a session when `ZENOH_LOCATOR` is set; otherwise `Ok(None)`.
pub async fn try_open_from_env() -> zenoh::Result<Option<Session>> {
    match locator_from_env() {
        None => Ok(None),
        Some(locator) => Ok(Some(open_session(&locator).await?)),
    }
}

/// Decode a subscriber payload as [`BusMessage`].
pub fn decode_sample(bytes: &[u8]) -> Result<BusMessage, serde_json::Error> {
    crate::codec::decode_json(bytes)
}

/// `put` a [`BusMessage`] as `application/json` on `key`.
pub async fn put_bus_message(session: &Session, key: &str, msg: &BusMessage) -> zenoh::Result<()> {
    let payload = encode_json(msg)?;
    session
        .put(key, payload)
        .encoding(Encoding::APPLICATION_JSON)
        .congestion_control(CongestionControl::Block)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_config_embeds_locator() {
        let cfg = client_config("tcp/127.0.0.1:7447").expect("json5");
        let endpoints = cfg
            .get_json("connect/endpoints")
            .expect("connect/endpoints");
        assert!(
            endpoints.contains("tcp/127.0.0.1:7447"),
            "endpoints={endpoints}"
        );
        let mode = cfg.get_json("mode").expect("mode");
        assert!(mode.contains("client"), "mode={mode}");
    }

    #[tokio::test]
    async fn live_connect_skips_without_zenohd() {
        let Some(locator) = locator_from_env() else {
            // Optional: no zenohd / ZENOH_LOCATOR in the default unit-test path.
            return;
        };
        let session = open_session(&locator)
            .await
            .unwrap_or_else(|e| panic!("ZENOH_LOCATOR={locator} but open failed: {e}"));
        session.close().await.unwrap();
    }
}
