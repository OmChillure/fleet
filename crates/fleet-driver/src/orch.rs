//! HTTP client for orchestrator worker heartbeat/status.

use anyhow::{Result, anyhow};
use reqwest::Client;
use serde_json::json;
use std::time::Duration;

#[derive(Clone)]
pub struct OrchClient {
    api_base: String,
    project_id: String,
    session_id: String,
    token: String,
    http: Client,
}

impl OrchClient {
    pub fn new(
        api_base: impl Into<String>,
        project_id: impl Into<String>,
        session_id: impl Into<String>,
        token: impl Into<String>,
    ) -> Result<Self> {
        let http = Client::builder().timeout(Duration::from_secs(30)).build()?;
        Ok(Self {
            api_base: api_base.into().trim_end_matches('/').to_string(),
            project_id: project_id.into(),
            session_id: session_id.into(),
            token: token.into(),
            http,
        })
    }

    fn worker_url(&self, tail: &str) -> String {
        format!(
            "{}/v1/projects/{}/workers/{}/{}",
            self.api_base, self.project_id, self.session_id, tail
        )
    }

    pub async fn post_heartbeat(
        &self,
        seq: i32,
        progress_pct: Option<i32>,
        events: &[serde_json::Value],
    ) -> Result<()> {
        let url = self.worker_url("heartbeat");
        let res = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(&json!({
                "seq": seq,
                "progress_pct": progress_pct,
                "events": events,
            }))
            .send()
            .await?;
        if !res.status().is_success() {
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            return Err(anyhow!("heartbeat {status}: {text}"));
        }
        Ok(())
    }

    pub async fn post_status(&self, seq: i32, status: &str, reason: Option<&str>) -> Result<()> {
        self.post_status_proof(seq, status, reason, None, None)
            .await
    }

    pub async fn post_status_proof(
        &self,
        seq: i32,
        status: &str,
        reason: Option<&str>,
        result_md: Option<&str>,
        files: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<()> {
        let url = self.worker_url("status");
        let result_md = result_md
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| truncate_reason(s, 256 * 1024));
        let res = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(&json!({
                "seq": seq,
                "status": status,
                "reason": reason,
                "result_md": result_md,
                "files": files,
            }))
            .send()
            .await?;
        if !res.status().is_success() {
            let code = res.status();
            let text = res.text().await.unwrap_or_default();
            return Err(anyhow!("status {code}: {text}"));
        }
        Ok(())
    }

    /// Worker I/O on the plane: integrations, team chat, request_changes.
    pub async fn post_io(&self, body: serde_json::Value) -> Result<serde_json::Value> {
        let url = self.worker_url("io");
        let res = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("io {status}: {text}"));
        }
        if text.trim().is_empty() {
            return Ok(json!({ "ok": true }));
        }
        serde_json::from_str(&text).or_else(|_| Ok(json!({ "ok": true, "raw": text })))
    }
}

pub fn truncate_reason(s: &str, max_chars: usize) -> String {
    let mut it = s.chars();
    let mut out: String = it.by_ref().take(max_chars).collect();
    if it.next().is_some() {
        out.push('…');
    }
    out
}
