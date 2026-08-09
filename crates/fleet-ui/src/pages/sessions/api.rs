//! Cookie-auth session list + live detail.

pub use crate::pages::fleet::api::FleetError;

use serde_json::Value;

use crate::pages::auth::api::auth_base_url;

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

fn api_url(path: &str) -> String {
    format!(
        "{}/{}",
        auth_base_url().trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

#[allow(unused_mut)]
fn with_credentials(mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    #[cfg(target_arch = "wasm32")]
    {
        req = req.fetch_credentials_include();
    }
    req
}

pub async fn get_session_detail(
    workspace_id: &str,
    session_id: &str,
) -> Result<Value, FleetError> {
    let res = with_credentials(client().get(api_url(&format!(
        "/api/workspaces/{workspace_id}/fleet/sessions/{session_id}"
    ))))
    .send()
    .await
    .map_err(|e| FleetError::Network(format!("could not reach API: {e}")))?;
    let status = res.status();
    let text = res
        .text()
        .await
        .map_err(|e| FleetError::Network(e.to_string()))?;
    if status.is_success() {
        serde_json::from_str(&text).map_err(|e| FleetError::Network(format!("invalid JSON: {e}")))
    } else {
        Err(FleetError::Message(if text.is_empty() {
            format!("request failed ({status})")
        } else {
            text
        }))
    }
}
