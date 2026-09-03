//! Live Zenoh subscribe + outbox drain. Opt-in via `ZENOH_LOCATOR`.
//! Never inject a locator into sandbox pods.

use std::time::Duration;

use fleet_bus::{
    decode_sample, key_for_message, put_bus_message, try_open_from_env,
    KEY_EXPR_ALL_SANDBOX_EVENTS, KEY_EXPR_ALL_SESSION_EVENTS,
};
use fleet_db::repos::control_plane_events;
use fleet_db::PgPool;
use tracing::{info, warn};

/// Start subscriber + outbox publisher when `ZENOH_LOCATOR` is set.
pub fn start_bus_tasks(pool: PgPool) {
    tokio::spawn(async move {
        let session = match try_open_from_env().await {
            Ok(Some(s)) => s,
            Ok(None) => {
                info!("ZENOH_LOCATOR unset; control-plane bus offline");
                return;
            }
            Err(e) => {
                warn!(error = %e, "zenoh open failed; control-plane bus offline");
                return;
            }
        };
        info!("control-plane bus connected");
        for key in [KEY_EXPR_ALL_SANDBOX_EVENTS, KEY_EXPR_ALL_SESSION_EVENTS] {
            let sub_session = session.clone();
            let sub_pool = pool.clone();
            tokio::spawn(async move {
                subscribe_loop(sub_session, sub_pool, key).await;
            });
        }
        drain_loop(session, pool).await;
    });
}

async fn subscribe_loop(session: zenoh::Session, pool: PgPool, key_expr: &'static str) {
    let Ok(sub) = session.declare_subscriber(key_expr).await else {
        warn!(key = key_expr, "zenoh subscriber declare failed");
        return;
    };
    loop {
        let sample = match sub.recv_async().await {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, key = key_expr, "zenoh recv ended");
                break;
            }
        };
        let bytes = sample.payload().to_bytes();
        let msg = match decode_sample(&bytes) {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "zenoh sample is not a BusMessage");
                continue;
            }
        };
        let body = match serde_json::to_value(&msg) {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "serialize inbound BusMessage");
                continue;
            }
        };
        if let Err(e) = control_plane_events::insert_inbound_published(
            &pool,
            msg.envelope.message_id.as_uuid(),
            msg.type_name(),
            body,
        )
        .await
        {
            warn!(error = %e, "persist inbound bus message");
        }
    }
}

async fn drain_loop(session: zenoh::Session, pool: PgPool) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let rows = match control_plane_events::list_unpublished(&pool, 64).await {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "list unpublished bus events");
                continue;
            }
        };
        for row in rows {
            let msg =
                match serde_json::from_value::<agent_fleet_contracts::BusMessage>(row.body.clone())
                {
                    Ok(m) => m,
                    Err(e) => {
                        warn!(error = %e, id = %row.id, "outbox row is not a BusMessage");
                        let _ = control_plane_events::mark_published(&pool, row.id).await;
                        continue;
                    }
                };
            let tenant = msg
                .envelope
                .tenant_id
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(fleet_bus::DEFAULT_TENANT);
            let Some(key) = key_for_message(tenant, &msg) else {
                let _ = control_plane_events::mark_published(&pool, row.id).await;
                continue;
            };
            if let Err(e) = put_bus_message(&session, &key, &msg).await {
                warn!(error = %e, key = %key, "zenoh put failed; will retry");
                continue;
            }
            if let Err(e) = control_plane_events::mark_published(&pool, row.id).await {
                warn!(error = %e, id = %row.id, "mark published failed");
            }
        }
    }
}
