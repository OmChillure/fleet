//! Control-plane integration bridge used by workers and run write-back.

use async_trait::async_trait;
use fleet_db::PgPool;
use fleet_integrations::proxy::ProxyCaller;
use fleet_integrations::CredentialsKey;
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
        let res = crate::workforce::gated_invoke_pool(
            &self.db,
            &self.key,
            workspace_id,
            ProxyCaller::System,
            &provider,
            &operation,
            args,
        )
        .await;
        IntegrationResult {
            ok: res.ok,
            summary: res.summary,
            data: res.data,
        }
    }
}
