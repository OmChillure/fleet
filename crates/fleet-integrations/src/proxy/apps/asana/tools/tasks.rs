//! Asana workspace / project / task tools.

use serde_json::{Value, json};

use super::super::client::*;
use crate::proxy::types::ProxyResult;

const TASK_FIELDS: &str = "gid,name,notes,completed,assignee.name,projects.gid,projects.name,due_on,permalink_url,modified_at";

pub(crate) async fn list_workspaces(
    http: &reqwest::Client,
    token: &str,
    _args: &Value,
) -> ProxyResult {
    let (status, body) = asana_get(http, token, "/workspaces?opt_fields=gid,name").await;
    if !(200..300).contains(&status) {
        return upstream_err("list_workspaces", status, &body);
    }
    let items: Vec<Value> = unwrap_data(&body)
        .as_array()
        .map(|arr| arr.iter().map(map_workspace).collect())
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "asana",
        "list_workspaces",
        format!("Listed {n} Asana workspaces"),
        json!({ "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn list_projects(
    http: &reqwest::Client,
    token: &str,
    args: &Value,
) -> ProxyResult {
    let workspace = args
        .get("workspace")
        .or_else(|| args.get("workspace_gid"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let mut path =
        "/projects?opt_fields=gid,name,archived,workspace.gid,permalink_url&limit=50".to_string();
    if let Some(ws) = workspace {
        path.push_str(&format!("&workspace={ws}"));
    }
    let (status, body) = asana_get(http, token, &path).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_projects", status, &body);
    }
    let items: Vec<Value> = unwrap_data(&body)
        .as_array()
        .map(|arr| arr.iter().map(map_project).collect())
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "asana",
        "list_projects",
        format!("Listed {n} Asana projects"),
        json!({ "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn list_tasks(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let project = args
        .get("project")
        .or_else(|| args.get("project_gid"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let workspace = args
        .get("workspace")
        .or_else(|| args.get("workspace_gid"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let mut path = format!("/tasks?opt_fields={TASK_FIELDS}&limit=50");
    if let Some(p) = project {
        path.push_str(&format!("&project={p}"));
    } else if let Some(ws) = workspace {
        path.push_str(&format!("&workspace={ws}&assignee=me"));
    } else {
        return ProxyResult::err(
            "asana",
            "list_tasks",
            "bad_args",
            "project or workspace is required",
            None,
        );
    }
    let (status, body) = asana_get(http, token, &path).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_tasks", status, &body);
    }
    let items: Vec<Value> = unwrap_data(&body)
        .as_array()
        .map(|arr| arr.iter().map(map_task).collect())
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "asana",
        "list_tasks",
        format!("Listed {n} Asana tasks"),
        json!({ "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn get_task(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let gid = match req_str(args, "task").or_else(|_| req_str(args, "gid")) {
        Ok(id) => id,
        Err(e) => return ProxyResult::err("asana", "get_task", "bad_args", e, None),
    };
    let (status, body) = asana_get(
        http,
        token,
        &format!("/tasks/{gid}?opt_fields={TASK_FIELDS}"),
    )
    .await;
    if !(200..300).contains(&status) {
        return upstream_err("get_task", status, &body);
    }
    ProxyResult::ok_result(
        "asana",
        "get_task",
        "Fetched Asana task",
        map_task(&unwrap_data(&body)),
        Some(status),
    )
}

pub(crate) async fn create_task(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let name = match req_str(args, "name").or_else(|_| req_str(args, "title")) {
        Ok(n) => n,
        Err(e) => return ProxyResult::err("asana", "create_task", "bad_args", e, None),
    };
    let mut data = json!({ "name": name });
    if let Some(notes) = args
        .get("notes")
        .or_else(|| args.get("description"))
        .and_then(|v| v.as_str())
    {
        data["notes"] = json!(notes);
    }
    if let Some(ws) = args
        .get("workspace")
        .or_else(|| args.get("workspace_gid"))
        .and_then(|v| v.as_str())
    {
        data["workspace"] = json!(ws);
    }
    if let Some(p) = args
        .get("project")
        .or_else(|| args.get("project_gid"))
        .and_then(|v| v.as_str())
    {
        data["projects"] = json!([p]);
    }
    if data.get("workspace").is_none() && data.get("projects").is_none() {
        return ProxyResult::err(
            "asana",
            "create_task",
            "bad_args",
            "workspace or project is required",
            None,
        );
    }
    let (status, body) = asana_post(http, token, "/tasks", &data).await;
    if !(200..300).contains(&status) {
        return upstream_err("create_task", status, &body);
    }
    ProxyResult::ok_result(
        "asana",
        "create_task",
        format!("Created Asana task `{name}`"),
        map_task(&unwrap_data(&body)),
        Some(status),
    )
}

pub(crate) async fn update_task(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let gid = match req_str(args, "task").or_else(|_| req_str(args, "gid")) {
        Ok(id) => id,
        Err(e) => return ProxyResult::err("asana", "update_task", "bad_args", e, None),
    };
    let mut data = json!({});
    if let Some(n) = args
        .get("name")
        .or_else(|| args.get("title"))
        .and_then(|v| v.as_str())
    {
        data["name"] = json!(n);
    }
    if let Some(notes) = args
        .get("notes")
        .or_else(|| args.get("description"))
        .and_then(|v| v.as_str())
    {
        data["notes"] = json!(notes);
    }
    if let Some(done) = args.get("completed").and_then(|v| v.as_bool()) {
        data["completed"] = json!(done);
    }
    if let Some(due) = args.get("due_on").and_then(|v| v.as_str()) {
        data["due_on"] = json!(due);
    }
    if data.as_object().map(|m| m.is_empty()).unwrap_or(true) {
        return ProxyResult::err(
            "asana",
            "update_task",
            "bad_args",
            "provide name, notes, completed, and/or due_on",
            None,
        );
    }
    let (status, body) = asana_put(http, token, &format!("/tasks/{gid}"), &data).await;
    if !(200..300).contains(&status) {
        return upstream_err("update_task", status, &body);
    }
    ProxyResult::ok_result(
        "asana",
        "update_task",
        "Updated Asana task",
        map_task(&unwrap_data(&body)),
        Some(status),
    )
}

pub(crate) async fn add_comment(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let gid = match req_str(args, "task").or_else(|_| req_str(args, "gid")) {
        Ok(id) => id,
        Err(e) => return ProxyResult::err("asana", "add_comment", "bad_args", e, None),
    };
    let text = match args
        .get("text")
        .or_else(|| args.get("body"))
        .or_else(|| args.get("comment"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(t) => t.to_string(),
        None => {
            return ProxyResult::err("asana", "add_comment", "bad_args", "text is required", None);
        }
    };
    let (status, body) = asana_post(
        http,
        token,
        &format!("/tasks/{gid}/stories"),
        &json!({ "text": text }),
    )
    .await;
    if !(200..300).contains(&status) {
        return upstream_err("add_comment", status, &body);
    }
    let story = unwrap_data(&body);
    ProxyResult::ok_result(
        "asana",
        "add_comment",
        "Added Asana comment",
        json!({
            "gid": story.get("gid"),
            "task": gid,
            "text": story.get("text").cloned().unwrap_or(json!(text)),
        }),
        Some(status),
    )
}
