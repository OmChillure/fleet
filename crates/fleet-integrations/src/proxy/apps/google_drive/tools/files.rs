//! Google Drive file list / metadata / export (readonly).

use serde_json::{json, Value};

use crate::proxy::types::ProxyResult;
use super::super::client::*;

const FILE_FIELDS: &str =
    "id,name,mimeType,modifiedTime,webViewLink,size,owners(displayName,emailAddress)";

pub(crate) async fn list_files(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let page_size = args
        .get("page_size")
        .or_else(|| args.get("limit"))
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 50);
    let q = args
        .get("q")
        .or_else(|| args.get("query"))
        .and_then(|v| v.as_str())
        .unwrap_or("trashed = false")
        .to_string();
    let q_enc = urlencoding(&q);
    let url = format!(
        "https://www.googleapis.com/drive/v3/files?pageSize={page_size}&q={q_enc}&fields=files({FILE_FIELDS}),nextPageToken&orderBy=modifiedTime desc"
    );
    let (status, body) = drive_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_files", status, &body);
    }
    let items: Vec<Value> = body
        .get("files")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().map(map_file).collect())
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "google_drive",
        "list_files",
        format!("Listed {n} Drive files"),
        json!({
            "count": n,
            "q": q,
            "nextPageToken": body.get("nextPageToken"),
            "items": items
        }),
        Some(status),
    )
}

pub(crate) async fn get_file(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let id = match req_str(args, "file_id").or_else(|_| req_str(args, "id")) {
        Ok(i) => i,
        Err(e) => return ProxyResult::err("google_drive", "get_file", "bad_args", e, None),
    };
    let url = format!(
        "https://www.googleapis.com/drive/v3/files/{id}?fields={FILE_FIELDS},description,createdTime"
    );
    let (status, body) = drive_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("get_file", status, &body);
    }
    ProxyResult::ok_result(
        "google_drive",
        "get_file",
        "Fetched Drive file metadata",
        map_file(&body),
        Some(status),
    )
}

pub(crate) async fn export_file(
    http: &reqwest::Client,
    token: &str,
    args: &Value,
) -> ProxyResult {
    let id = match req_str(args, "file_id").or_else(|_| req_str(args, "id")) {
        Ok(i) => i,
        Err(e) => return ProxyResult::err("google_drive", "export_file", "bad_args", e, None),
    };
    let meta_url = format!("https://www.googleapis.com/drive/v3/files/{id}?fields={FILE_FIELDS}");
    let (status, meta) = drive_get(http, token, &meta_url).await;
    if !(200..300).contains(&status) {
        return upstream_err("export_file", status, &meta);
    }
    let mime = meta
        .get("mimeType")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let export_mime = args
        .get("mime_type")
        .and_then(|v| v.as_str())
        .unwrap_or(match mime {
            "application/vnd.google-apps.document" => "text/plain",
            "application/vnd.google-apps.spreadsheet" => "text/csv",
            "application/vnd.google-apps.presentation" => "text/plain",
            _ => "",
        });
    if export_mime.is_empty() {
        return ProxyResult::ok_result(
            "google_drive",
            "export_file",
            "File is not a Google Doc/Sheet/Slide — returning metadata only",
            json!({
                "file": map_file(&meta),
                "note": "Use mime_type for Google native files, or download is not supported for binary blobs in this proxy.",
            }),
            Some(status),
        );
    }
    let exp_enc = urlencoding(export_mime);
    let url = format!(
        "https://www.googleapis.com/drive/v3/files/{id}/export?mimeType={exp_enc}"
    );
    let (status, body) = drive_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("export_file", status, &body);
    }
    let text = body
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    ProxyResult::ok_result(
        "google_drive",
        "export_file",
        format!("Exported Drive file {}", meta.get("name").and_then(|v| v.as_str()).unwrap_or(&id)),
        json!({
            "file": map_file(&meta),
            "mime_type": export_mime,
            "text": text,
        }),
        Some(status),
    )
}

fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
