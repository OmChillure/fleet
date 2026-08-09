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

    pub async fn post_heartbeat(&self, seq: i32, progress_pct: Option<i32>) -> Result<()> {
        let url = self.worker_url("heartbeat");
        let res = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(&json!({ "seq": seq, "progress_pct": progress_pct }))
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
        let url = self.worker_url("status");
        let res = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(&json!({
                "seq": seq,
                "status": status,
                "reason": reason,
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
}

pub fn truncate_reason(s: &str, max_chars: usize) -> String {
    let mut it = s.chars();
    let mut out: String = it.by_ref().take(max_chars).collect();
    if it.next().is_some() {
        out.push('…');
    }
    out
}
