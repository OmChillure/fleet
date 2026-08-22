//! Control-plane integration bridge used by workers and run write-back.

use async_trait::async_trait;
use fleet_db::PgPool;
use fleet_integrations::CredentialsKey;
use fleet_integrations::proxy::{self, ProxyCaller, ProxyRequest};
use fleet_orchestrator::sandbox::io::{IntegrationInvoker, IntegrationResult};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone)]
pub struct ControlPlaneIo {
    pub db: PgPool,
    pub key: CredentialsKey,
}

#[async_trait]
impl IntegrationInvoker for ControlPlaneIo {
    async fn invoke(
        &self,
        workspace_id: Uuid,
        provider: String,
        operation: String,
        args: Value,
    ) -> IntegrationResult {
        let res = proxy::invoke(
            &self.db,
            &self.key,
            ProxyRequest {
                workspace_id,
                provider,
                operation,
                args,
                caller: ProxyCaller::System,
            },
        )
        .await;
        IntegrationResult {
            ok: res.ok,
            summary: res.summary,
            data: res.data,
        }
    }
}
