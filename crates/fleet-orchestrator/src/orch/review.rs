//! Reviewer bounce: send an implementer back to `ready` with notes.

use crate::sandbox::share::ChangesRequest;
use crate::sandbox::team_chat::{self, TeamTalk};
use crate::store::{BoardSnapshot, Store};
use uuid::Uuid;

use super::tick::TickError;

pub async fn apply_review_bounce(
    store: &Store,
    snap: &BoardSnapshot,
    reviewer_task_id: Uuid,
    req: &ChangesRequest,
    data_dir: &std::path::Path,
) -> Result<usize, TickError> {
    let Some(reviewer) = snap.tasks.iter().find(|t| t.id == reviewer_task_id) else {
        return Ok(0);
    };
    let id_to_plan: std::collections::HashMap<Uuid, String> = snap
        .tasks
        .iter()
        .filter(|t| t.run_id == reviewer.run_id)
        .map(|t| (t.id, t.plan_task_id.clone()))
        .collect();
    let plan_to_task: std::collections::HashMap<String, Uuid> = snap
        .tasks
        .iter()
        .filter(|t| t.run_id == reviewer.run_id)
        .map(|t| (t.plan_task_id.to_ascii_lowercase(), t.id))
        .collect();

    let mut targets: Vec<Uuid> = req
        .targets
        .iter()
        .filter_map(|s| plan_to_task.get(&s.trim().to_ascii_lowercase()).copied())
        .collect();
    if targets.is_empty() {
        for dep in &reviewer.depends_on {
            if id_to_plan.contains_key(dep) {
                targets.push(*dep);
            }
        }
    }
    targets.retain(|id| *id != reviewer_task_id);
    if targets.is_empty() {
        return Ok(0);
    }

    let reason = req.reason.trim();
    let note = format!(
        "## Changes requested by `{}` ({})\n\n{reason}\n",
        reviewer.plan_task_id, reviewer.worker_profile
    );

    for id in &targets {
        store
            .reopen_task_for_changes(*id, "changes_requested", &note)
            .await?;
    }

    // Reviewer waits on those deps again.
    store
        .requeue_task_waiting(reviewer_task_id, "waiting_on_changes")
        .await?;

    // Anyone else who depended on a bounced task and already left queued
    // must wait again (except we just requeued the reviewer).
    let bounced: std::collections::HashSet<Uuid> = targets.iter().copied().collect();
    for t in &snap.tasks {
        if t.run_id != reviewer.run_id || t.id == reviewer_task_id {
            continue;
        }
        if bounced.contains(&t.id) {
            continue;
        }
        if !t.depends_on.iter().any(|d| bounced.contains(d)) {
            continue;
        }
        if matches!(
            t.status.as_str(),
            "ready" | "assigned" | "in_progress" | "retry_wait" | "done_success"
        ) {
            store
                .requeue_task_waiting(t.id, "waiting_on_changes")
                .await?;
        }
    }

    let talk = TeamTalk::from_spawn(
        data_dir,
        reviewer.project_id,
        reviewer.run_id,
        reviewer.plan_task_id.clone(),
        reviewer.worker_profile.clone(),
    );
    let to = targets
        .first()
        .and_then(|id| id_to_plan.get(id))
        .map(|s| s.as_str())
        .unwrap_or("*");
    let _ = team_chat::post_message(
        &talk,
        to,
        "ask",
        &format!("Changes requested:\n{reason}"),
        None,
    )
    .await;

    Ok(targets.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_request_deserializes() {
        let r: ChangesRequest =
            serde_json::from_str(r#"{"targets":["t1"],"reason":"add tests"}"#).unwrap();
        assert_eq!(r.targets[0], "t1");
    }
}
