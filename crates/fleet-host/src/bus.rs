//! Optional Zenoh publish from this host. No-op when `ZENOH_LOCATOR` is unset.

use agent_fleet_contracts::{
    BusMessage, MessageBody, Producer, SessionCancelled, SessionCompleted, SessionFailed,
    SessionId, SessionState, SessionStateChanged,
};
use fleet_bus::{
    DEFAULT_TENANT, key_for_message, put_bus_message, try_open_from_env,
};
use tracing::{info, warn};
use uuid::Uuid;
use zenoh::Session;

#[derive(Clone)]
pub struct BusHandle {
    session: Option<Session>,
}

impl BusHandle {
    pub async fn from_env() -> Self {
        match try_open_from_env().await {
            Ok(Some(session)) => {
                info!("fleet-host bus connected");
                Self {
                    session: Some(session),
                }
            }
            Ok(None) => Self { session: None },
            Err(e) => {
                warn!(error = %e, "fleet-host bus open failed");
                Self { session: None }
            }
        }
    }

    pub async fn session_started(&self, session_id: Uuid) {
        let sid = SessionId::from_uuid(session_id);
        self.publish(BusMessage::new(
            Producer::Other("fleet-host".into()),
            MessageBody::SessionStateChanged(SessionStateChanged {
                session_id: sid,
                from: SessionState::AwaitingSandbox,
                to: SessionState::Running,
                reason: Some("leased".into()),
            }),
        ))
        .await;
    }

    pub async fn session_finished(&self, session_id: Uuid, ok: bool, error: Option<&str>) {
        let sid = SessionId::from_uuid(session_id);
        let body = if ok {
            MessageBody::SessionCompleted(SessionCompleted {
                session_id: sid,
                summary: None,
            })
        } else {
            MessageBody::SessionFailed(SessionFailed {
                session_id: sid,
                message: error.unwrap_or("job failed").to_string(),
                reason_code: None,
            })
        };
        self.publish(BusMessage::new(Producer::Other("fleet-host".into()), body))
            .await;
    }

    pub async fn session_cancelled(&self, session_id: Uuid) {
        let sid = SessionId::from_uuid(session_id);
        self.publish(BusMessage::new(
            Producer::Other("fleet-host".into()),
            MessageBody::SessionCancelled(SessionCancelled {
                session_id: sid,
                reason: Some("signal_session".into()),
            }),
        ))
        .await;
    }

    async fn publish(&self, mut msg: BusMessage) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        if msg.envelope.tenant_id.is_none() {
            msg.envelope.tenant_id = Some(DEFAULT_TENANT.into());
        }
        let Some(key) = key_for_message(DEFAULT_TENANT, &msg) else {
            return;
        };
        if let Err(e) = put_bus_message(session, &key, &msg).await {
            warn!(error = %e, key = %key, "fleet-host bus put failed");
        }
    }
}
