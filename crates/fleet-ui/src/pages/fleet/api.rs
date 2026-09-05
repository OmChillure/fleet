//! Cookie-auth fleet API under `/api/workspaces/{workspace_id}/fleet/*`.

use serde::Deserialize;
use serde_json::Value;

use crate::pages::auth::api::{auth_base_url, with_credentials};

#[derive(Debug, Clone)]
pub enum FleetError {
    Message(String),
    Network(String),
}

impl FleetError {
    pub fn message(&self) -> String {
        match self {
            Self::Message(m) | Self::Network(m) => m.clone(),
        }
    }
}

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

/// POST body for Fleet → Start 2-task run (`…/fleet/runs/fixture`).
///
/// Docs §6.5: UI sends `kind=chain` only. `on_task_fail=fail_run`.
/// Starting the fixture does not tick.
pub fn start_fixture_body() -> Value {
    serde_json::json!({
        "kind": "chain",
        "on_task_fail": "fail_run",
    })
}


async fn parse(res: reqwest::Response) -> Result<Value, FleetError> {
    let status = res.status();
    let text = res
        .text()
        .await
        .map_err(|e| FleetError::Network(e.to_string()))?;
    if status.is_success() {
        if text.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text)
            .map_err(|e| FleetError::Network(format!("invalid JSON: {e} ({text})")))
    } else {
        #[derive(Deserialize)]
        struct ErrBody {
            error: String,
        }
        let msg = serde_json::from_str::<ErrBody>(&text)
            .map(|b| b.error)
            .unwrap_or_else(|_| {
                if text.is_empty() {
                    format!("request failed ({status})")
                } else {
                    text
                }
            });
        Err(FleetError::Message(msg))
    }
}

pub async fn ensure_fleet(workspace_id: &str) -> Result<Value, FleetError> {
    let res =
        with_credentials(client().get(api_url(&format!("/api/workspaces/{workspace_id}/fleet"))))
            .send()
            .await
            .map_err(|e| FleetError::Network(format!("could not reach API: {e}")))?;
    parse(res).await
}

pub async fn start_fixture(workspace_id: &str) -> Result<Value, FleetError> {
    let res = with_credentials(
        client()
            .post(api_url(&format!(
                "/api/workspaces/{workspace_id}/fleet/runs/fixture"
            )))
            .json(&start_fixture_body()),
    )
    .send()
    .await
    .map_err(|e| FleetError::Network(format!("could not reach API: {e}")))?;
    parse(res).await
}

pub async fn run_tick(workspace_id: &str) -> Result<Value, FleetError> {
    let res = with_credentials(
        client()
            .post(api_url(&format!(
                "/api/workspaces/{workspace_id}/fleet/tick"
            )))
            .json(&serde_json::json!({})),
    )
    .send()
    .await
    .map_err(|e| FleetError::Network(format!("could not reach API: {e}")))?;
    parse(res).await
}

pub async fn list_tasks(workspace_id: &str) -> Result<Value, FleetError> {
    let res = with_credentials(client().get(api_url(&format!(
        "/api/workspaces/{workspace_id}/fleet/tasks"
    ))))
    .send()
    .await
    .map_err(|e| FleetError::Network(format!("could not reach API: {e}")))?;
    parse(res).await
}

pub async fn list_runs(workspace_id: &str) -> Result<Value, FleetError> {
    let res = with_credentials(client().get(api_url(&format!(
        "/api/workspaces/{workspace_id}/fleet/runs"
    ))))
    .send()
    .await
    .map_err(|e| FleetError::Network(format!("could not reach API: {e}")))?;
    parse(res).await
}

pub async fn list_sessions(workspace_id: &str) -> Result<Value, FleetError> {
    let res = with_credentials(client().get(api_url(&format!(
        "/api/workspaces/{workspace_id}/fleet/sessions"
    ))))
    .send()
    .await
    .map_err(|e| FleetError::Network(format!("could not reach API: {e}")))?;
    parse(res).await
}

pub async fn resume_task(
    workspace_id: &str,
    task_id: &str,
    message: &str,
) -> Result<Value, FleetError> {
    let res = with_credentials(
        client()
            .post(api_url(&format!(
                "/api/workspaces/{workspace_id}/fleet/tasks/{task_id}/resume"
            )))
            .json(&serde_json::json!({ "message": message })),
    )
    .send()
    .await
    .map_err(|e| FleetError::Network(format!("could not reach API: {e}")))?;
    parse(res).await
}

pub async fn replace_session(workspace_id: &str, session_id: &str) -> Result<Value, FleetError> {
    let res = with_credentials(
        client()
            .post(api_url(&format!(
                "/api/workspaces/{workspace_id}/fleet/sessions/{session_id}/replace"
            )))
            .json(&serde_json::json!({ "reason": "ui_replace" })),
    )
    .send()
    .await
    .map_err(|e| FleetError::Network(format!("could not reach API: {e}")))?;
    parse(res).await
}

pub async fn kill_session(workspace_id: &str, session_id: &str) -> Result<Value, FleetError> {
    let res = with_credentials(
        client()
            .post(api_url(&format!(
                "/api/workspaces/{workspace_id}/fleet/sessions/{session_id}/kill"
            )))
            .json(&serde_json::json!({ "reason": "ui_kill" })),
    )
    .send()
    .await
    .map_err(|e| FleetError::Network(format!("could not reach API: {e}")))?;
    parse(res).await
}

pub async fn cancel_run(workspace_id: &str, run_id: &str) -> Result<Value, FleetError> {
    let res = with_credentials(
        client()
            .post(api_url(&format!(
                "/api/workspaces/{workspace_id}/fleet/runs/{run_id}/cancel"
            )))
            .json(&serde_json::json!({ "reason": "ui_stop" })),
    )
    .send()
    .await
    .map_err(|e| FleetError::Network(format!("could not reach API: {e}")))?;
    parse(res).await
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    // catalog:list-fleet-ui-fixture-is-chain-not-diamond
    // Docs §6.5: Start 2-task run POSTs {kind: "chain", on_task_fail: "fail_run"}.
    // UI never sends diamond. Does not tick.
    // Host: cargo test -p fleet-ui --locked -- chain

    fn production_api() -> &'static str {
        include_str!("api.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("fleet api.rs production source")
    }

    fn rust_item_src<'a>(src: &'a str, sig: &str) -> &'a str {
        let start = src.find(sig).unwrap_or_else(|| panic!("missing {sig}"));
        let after = &src[start + sig.len()..];
        let brace = after
            .find('{')
            .unwrap_or_else(|| panic!("{sig} has no body"));
        let body = &after[brace..];
        let end = match_brace_end(body);
        &src[start..start + sig.len() + brace + end]
    }

    fn match_brace_end(body: &str) -> usize {
        let bytes = body.as_bytes();
        assert_eq!(bytes.first().copied(), Some(b'{'), "expected '{{' body");
        let mut depth = 0i32;
        for (i, &b) in bytes.iter().enumerate() {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return i + 1;
                    }
                }
                _ => {}
            }
        }
        panic!("unbalanced '{{' in function body");
    }

    fn strip_trailing_commas(s: &str) -> String {
        let chars: Vec<char> = s.chars().collect();
        let mut out = String::with_capacity(s.len());
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == ',' {
                let mut j = i + 1;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
                    i += 1;
                    continue;
                }
            }
            out.push(chars[i]);
            i += 1;
        }
        out
    }

    fn rust_json_macro_to_value(src: &str) -> Value {
        let idx = src
            .find("json!({")
            .unwrap_or_else(|| panic!("expected json!({{ ... }}) in:\n{src}"));
        let after = &src[idx + "json!(".len()..];
        let end = match_brace_end(after);
        let raw = strip_trailing_commas(&after[..end]);
        serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("json! body must be object-shaped JSON ({e}): {raw}"))
    }

    fn posted_start_fixture_body(api: &str) -> Value {
        let start = rust_item_src(api, "pub async fn start_fixture");
        assert!(
            start.contains("/fleet/runs/fixture"),
            "start_fixture must POST …/fleet/runs/fixture:\n{start}"
        );
        if start.contains("start_fixture_body()") {
            let builder_at = api.find("fn start_fixture_body").unwrap_or_else(|| {
                panic!("start_fixture calls start_fixture_body() but it is missing")
            });
            let builder = rust_item_src(
                &api[builder_at.saturating_sub(16)..],
                "fn start_fixture_body",
            );
            return rust_json_macro_to_value(builder);
        }
        rust_json_macro_to_value(start)
    }

    fn assert_chain_not_diamond_fixture(body: &Value, where_: &str) {
        assert_eq!(
            body.get("kind").and_then(Value::as_str),
            Some("chain"),
            "{where_} must post kind=chain (empty {{}} is not enough; UI must not omit kind): {body}"
        );
        assert_ne!(
            body.get("kind").and_then(Value::as_str),
            Some("diamond"),
            "{where_} must never send kind=diamond: {body}"
        );
        assert_eq!(
            body.get("on_task_fail").and_then(Value::as_str),
            Some("fail_run"),
            "{where_} must post on_task_fail=fail_run: {body}"
        );
        let dumped = serde_json::to_string(body).expect("serialize fixture body");
        assert!(
            !dumped.contains("diamond"),
            "{where_} payload must not mention diamond: {dumped}"
        );
    }

    #[test]
    fn start_2_task_fixture_body_is_chain_not_diamond() {
        let expected = super::start_fixture_body();
        assert_chain_not_diamond_fixture(&expected, "docs §6.5 Start 2-task body");
        assert_eq!(expected["kind"], "chain");
        assert_eq!(expected["on_task_fail"], "fail_run");
        assert_ne!(expected["kind"], "diamond");
        let diamond = serde_json::json!({
            "kind": "diamond",
            "on_task_fail": "fail_run",
        });
        assert_ne!(
            expected, diamond,
            "UI payload builder must not be the API diamond fixture"
        );
    }

    #[test]
    fn start_fixture_posts_kind_chain_never_diamond() {
        let api = production_api();
        let start = rust_item_src(api, "pub async fn start_fixture");
        let body = posted_start_fixture_body(api);
        assert_chain_not_diamond_fixture(&body, "start_fixture POST");
        assert_eq!(
            body,
            super::start_fixture_body(),
            "start_fixture must POST the §6.5 chain fixture body, got {body}"
        );
        assert!(
            !start.to_ascii_lowercase().contains("diamond"),
            "start_fixture must never mention diamond:\n{start}"
        );
        assert!(
            !start.contains("/tick") && !start.contains("run_tick"),
            "docs §6.5: Start 2-task must not tick:\n{start}"
        );
        assert!(
            !start.contains("/admin") && !start.contains("WorkPlanBody"),
            "UI fixture is POST /fleet/runs/fixture, not an admin work plan:\n{start}"
        );
        let tick = rust_item_src(api, "pub async fn run_tick");
        assert!(
            tick.contains("/fleet/tick"),
            "run_tick is a separate POST …/fleet/tick"
        );
        assert!(
            !tick.contains("/runs/fixture"),
            "run_tick must not start a fixture"
        );
    }
}
