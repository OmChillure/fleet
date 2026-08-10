//! Register Google Drive tools in OPERATIONS + match below.

mod files;

use serde_json::Value;

use crate::proxy::types::ProxyResult;

pub const OPERATIONS: &[&str] = &["list_files", "get_file", "export_file"];

pub async fn invoke(
    http: &reqwest::Client,
    token: &str,
    operation: &str,
    args: &Value,
) -> ProxyResult {
    match operation {
        "list_files" => files::list_files(http, token, args).await,
        "get_file" => files::get_file(http, token, args).await,
        "export_file" => files::export_file(http, token, args).await,
        other => ProxyResult::err(
            "google_drive",
            other,
            "unknown_op",
            format!(
                "unknown google_drive operation `{other}`; valid: {}",
                OPERATIONS.join(", ")
            ),
            None,
        ),
    }
}
