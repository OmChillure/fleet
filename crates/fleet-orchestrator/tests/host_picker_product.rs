//! Product contracts for host assignment: lowest load/cap, oldest heartbeat,
//! min_isolation, no loopback for off-box host-runtime, no loopback for
//! container guests, plan LLM → FLEET_HARNESS (Ollama fixture, not a cloud SKU).
//!
//! Required by `just test-integration` / merge-to-main. Requires DATABASE_URL.

use chrono::{Duration as ChronoDuration, Utc};
use fleet_db::models::NewWorkerHost;
use fleet_db::repos::worker_hosts::{self, generate_pair_token, hash_token};
use fleet_db::repos::workspaces;
use fleet_orchestrator::domain::{
    OnTaskFailPolicy, PlanPolicy, PlanTask, WorkPlanBody, WorkPlanStatus,
};
use fleet_orchestrator::orch::TickRunner;
use fleet_orchestrator::store::{
    connect, host_is_this_machine, migrate, select_assignable_host,
    select_assignable_runner_preferring, AssignableHost, BoardTaskRow, Store,
};
use uuid::Uuid;

fn require_database_url() -> String {
    std::env::var("DATABASE_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            panic!(
                "DATABASE_URL is required for this integration test. \
                 Start Postgres (just db-up) and export DATABASE_URL."
            )
        })
}

async fn setup() -> (Store, std::sync::MutexGuard<'static, ()>) {
    // Shared DATABASE_URL + cluster-visible hosts: serialize so one test's
    // this-machine cluster fixture cannot steal another test's pick.
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    let guard = LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let url = require_database_url();
    let pool = connect(&url).await.expect("connect");
    migrate(&pool).await.expect("orch migrate");
    fleet_db::migrate(&pool).await.expect("fleet-db migrate");
    let store = Store::new(pool);
    if let Ok(cluster) = worker_hosts::list_cluster(store.pool()).await {
        for h in cluster {
            if !h.paused {
                let _ = sqlx::query("UPDATE worker_hosts SET paused = true WHERE id = $1")
                    .bind(h.id)
                    .execute(store.pool())
                    .await;
            }
        }
    }
    (store, guard)
}

fn runner(store: Store, orch_url: &str) -> TickRunner {
    TickRunner::new(
        store,
        std::env::temp_dir().join("fleet-orch-picker-product"),
        orch_url,
    )
}

fn one_task_plan(project_id: Uuid) -> WorkPlanBody {
    n_task_plan(project_id, 1)
}

fn n_task_plan(project_id: Uuid, n: usize) -> WorkPlanBody {
    let tasks = (1..=n)
        .map(|i| PlanTask {
            plan_task_id: format!("t{i}"),
            title: format!("T{i}"),
            summary: String::new(),
            spec_md: String::new(),
            depends_on: vec![],
            worker_profile: "coding".into(),
            acceptance: vec![],
            priority: 100,
            external_ref: None,
            timeout_s: Some(3600),
            max_retries: Some(0),
        })
        .collect();
    WorkPlanBody {
        plan_id: Uuid::new_v4(),
        project_id,
        run_id: Uuid::new_v4(),
        version: 1,
        status: WorkPlanStatus::Accepted,
        source: "fixture".into(),
        tasks,
        policy: PlanPolicy {
            max_agents_override: Some(8),
            on_task_fail: OnTaskFailPolicy::FailRun,
            auto_sync_external: false,
            ..Default::default()
        },
    }
}

async fn seed_workspace(store: &Store, project_id: Uuid) {
    let slug = format!("pp{}", project_id.as_simple());
    sqlx::query(
        r#"
        INSERT INTO workspaces (id, slug, name, created_at, updated_at)
        VALUES ($1, $2, $3, now(), now())
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .bind(project_id)
    .bind(&slug)
    .bind("picker-product")
    .execute(store.pool())
    .await
    .expect("insert workspace");
}

async fn pair_host(
    store: &Store,
    workspace_id: Uuid,
    name: &str,
    advertised_host: bool,
    advertised_container: bool,
) -> Uuid {
    let token = generate_pair_token();
    let host = worker_hosts::create_pending(
        store.pool(),
        &NewWorkerHost::new(Uuid::now_v7(), workspace_id, name.to_string(), None),
        &hash_token(&token),
        Utc::now() + ChronoDuration::hours(1),
    )
    .await
    .expect("create_pending");
    worker_hosts::consume_pair_token(
        store.pool(),
        &token,
        name,
        "linux-x86_64",
        advertised_host,
        advertised_container,
        None,
    )
    .await
    .expect("pair");
    worker_hosts::heartbeat(
        store.pool(),
        host.id,
        advertised_host,
        advertised_container,
        Some("linux-x86_64"),
        Some(name),
        None,
        0,
        Some(4),
        Some("abc"),
        None,
        Some(false),
        None,
    )
    .await
    .expect("heartbeat");
    host.id
}

async fn age_last_seen(store: &Store, host_id: Uuid, seconds: i64) {
    sqlx::query(&format!(
        "UPDATE worker_hosts SET last_seen_at = now() - interval '{seconds} seconds' WHERE id = $1"
    ))
    .bind(host_id)
    .execute(store.pool())
    .await
    .expect("age last_seen");
}

fn require_this_machine_hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| panic!("this-machine fixture needs HOSTNAME or /etc/hostname"))
}

fn require_local_nonloopback_ip() -> String {
    if let Ok(output) = std::process::Command::new("hostname").arg("-I").output() {
        if output.status.success() {
            for tok in String::from_utf8_lossy(&output.stdout).split_whitespace() {
                if tok != "127.0.0.1" && tok != "::1" {
                    return tok.to_string();
                }
            }
        }
    }
    panic!("advertised-IP fixture needs a non-loopback address from hostname -I");
}

async fn stamp_hardware_addrs(store: &Store, host_id: Uuid, addrs: &[&str]) {
    sqlx::query("UPDATE worker_hosts SET hardware = $2 WHERE id = $1")
        .bind(host_id)
        .bind(serde_json::json!({ "addrs": addrs }))
        .execute(store.pool())
        .await
        .expect("stamp hardware.addrs");
}

async fn clear_host_identity(store: &Store, host_id: Uuid) {
    sqlx::query("UPDATE worker_hosts SET hostname = NULL, hardware = NULL WHERE id = $1")
        .bind(host_id)
        .execute(store.pool())
        .await
        .expect("clear hostname and hardware");
}

async fn leftover_this_machine_ids(
    store: &Store,
    project_id: Uuid,
    except: &[Uuid],
) -> Vec<Uuid> {
    let listed = worker_hosts::list_assignable(
        store.pool(),
        project_id,
        fleet_db::WORKER_HOST_OFFLINE_AFTER,
    )
    .await
    .expect("list_assignable");
    listed
        .iter()
        .filter(|h| !except.contains(&h.id) && host_is_this_machine(&AssignableHost::from(*h)))
        .map(|h| h.id)
        .collect()
}

/// Cross-workspace this-machine boxes the picker merges in (live fleet-host).
async fn leftover_this_machine_container(
    store: &Store,
    project_id: Uuid,
    except: &[Uuid],
) -> bool {
    leftover_cross_ws_this_machine(store, project_id, except)
        .await
        .iter()
        .any(|h| h.effective_container())
}

async fn leftover_cross_ws_this_machine(
    store: &Store,
    project_id: Uuid,
    except: &[Uuid],
) -> Vec<fleet_db::models::WorkerHost> {
    let host = require_this_machine_hostname();
    let mut names = vec![host.clone()];
    if let Some((short, _)) = host.split_once('.') {
        if !short.is_empty() {
            names.push(short.to_string());
        }
    }
    let extra = worker_hosts::list_assignable_by_hostnames(
        store.pool(),
        fleet_db::WORKER_HOST_OFFLINE_AFTER,
        &names,
    )
    .await
    .unwrap_or_default();
    extra
        .into_iter()
        .filter(|h| {
            h.workspace_id != project_id
                && !except.contains(&h.id)
                && host_is_this_machine(&AssignableHost::from(h))
        })
        .collect()
}

async fn materialize_ready_tasks(store: &Store, project_id: Uuid, n: usize) -> Vec<BoardTaskRow> {
    let plan = n_task_plan(project_id, n);
    let plan_id = plan.plan_id;
    store.insert_accepted_plan(project_id, plan).await.unwrap();
    store
        .materialize_plan(project_id, plan_id)
        .await
        .expect("materialize");
    let tasks = store.list_tasks_slim(project_id, None).await.unwrap();
    for t in &tasks {
        let _ = store
            .update_task_status(t.id, "queued", "ready", None)
            .await;
    }
    tasks
}

async fn enqueue_remote_on_host(store: &Store, task: &BoardTaskRow, host_id: Uuid) -> Uuid {
    let assignment = store
        .begin_spawn_assignment(
            task.id,
            task.project_id,
            task.run_id,
            &task.worker_profile,
            "remote",
            None,
        )
        .await
        .unwrap()
        .expect("cas");
    store
        .record_remote_enqueued(assignment.session_id, &format!("host:{host_id}"), None)
        .await
        .unwrap();
    assignment.session_id
}

#[tokio::test]
async fn picker_prefers_idle_host_over_busier_host_listed_first() {
    let (store, _picker_lock) = setup().await;
    let project = store
        .create_project(&format!("pp-load-{}", Uuid::new_v4()), 8)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    let busy = pair_host(&store, project.id, "busy", true, false).await;
    let idle = pair_host(&store, project.id, "idle", true, false).await;
    // Age busy (older last_seen). Idle stays newest / listed first. Without
    // SQL load (empty count_remote_in_flight) oldest-heartbeat would pick busy.
    age_last_seen(&store, busy, 20).await;

    let tasks = materialize_ready_tasks(&store, project.id, 3).await;
    for t in &tasks {
        enqueue_remote_on_host(&store, t, busy).await;
    }

    let chosen = select_assignable_host(&store, project.id)
        .await
        .expect("idle host must still be assignable");
    assert_eq!(
        chosen.id(),
        idle,
        "3 remote in-flight on the older host must lose to idle (SQL load/cap; oldest last_seen would pick busy)"
    );
}

#[tokio::test]
async fn picker_remote_in_flight_ignores_terminal_and_local() {
    let (store, _picker_lock) = setup().await;
    let project = store
        .create_project(&format!("pp-inflight-filter-{}", Uuid::new_v4()), 16)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    let busy = pair_host(&store, project.id, "busy", true, false).await;
    let idle = pair_host(&store, project.id, "idle", true, false).await;
    age_last_seen(&store, busy, 20).await;

    // 3 remote spawning on busy; completed/failed remote + local spawning on
    // idle. Counting those extras would tie or beat busy and oldest would win.
    let tasks = materialize_ready_tasks(&store, project.id, 9).await;
    for t in &tasks[..3] {
        enqueue_remote_on_host(&store, t, busy).await;
    }
    let completed = enqueue_remote_on_host(&store, &tasks[3], idle).await;
    let completed2 = enqueue_remote_on_host(&store, &tasks[4], idle).await;
    let failed = enqueue_remote_on_host(&store, &tasks[5], idle).await;
    sqlx::query("UPDATE worker_sessions SET status = 'completed' WHERE id IN ($1, $2)")
        .bind(completed)
        .bind(completed2)
        .execute(store.pool())
        .await
        .expect("mark completed");
    sqlx::query("UPDATE worker_sessions SET status = 'failed' WHERE id = $1")
        .bind(failed)
        .execute(store.pool())
        .await
        .expect("mark failed");
    for t in &tasks[6..] {
        let assignment = store
            .begin_spawn_assignment(
                t.id,
                t.project_id,
                t.run_id,
                &t.worker_profile,
                "local",
                None,
            )
            .await
            .unwrap()
            .expect("cas");
        sqlx::query("UPDATE worker_sessions SET runtime_handle = $2 WHERE id = $1")
            .bind(assignment.session_id)
            .bind(format!("host:{idle}"))
            .execute(store.pool())
            .await
            .expect("stamp local handle");
    }

    let chosen = select_assignable_host(&store, project.id)
        .await
        .expect("idle host must still be assignable");
    assert_eq!(
        chosen.id(),
        idle,
        "count_remote_in_flight must ignore completed/failed and runtime_backend=local (idle would lose if those rows counted)"
    );
}

#[tokio::test]
async fn picker_equal_load_home_workspace_beats_older_cluster() {
    let (store, _picker_lock) = setup().await;
    let project = store
        .create_project(&format!("pp-home-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    let other = store
        .create_project(&format!("pp-cluster-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, other.id).await;
    let home = pair_host(&store, project.id, "home", true, false).await;
    let cluster = pair_host(&store, other.id, "cluster-box", true, false).await;
    sqlx::query(
        "UPDATE worker_hosts SET labels = array['cluster'], run_mode = 'in_container' WHERE id = $1",
    )
        .bind(cluster)
        .execute(store.pool())
        .await
        .expect("stamp cluster");
    // Both 0/4. Cluster is older; without prefer_workspace oldest last_seen wins.
    age_last_seen(&store, cluster, 20).await;

    let chosen = select_assignable_host(&store, project.id)
        .await
        .expect("home host must be assignable");
    assert_eq!(
        chosen.id(),
        home,
        "equal load: home workspace beats an older cluster-label host"
    );
}

#[tokio::test]
async fn picker_load_ratio_uses_advertised_cap() {
    let (store, _picker_lock) = setup().await;
    let project = store
        .create_project(&format!("pp-cap-ratio-{}", Uuid::new_v4()), 16)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    let host_a = pair_host(&store, project.id, "cap2", true, false).await;
    let host_b = pair_host(&store, project.id, "cap8", true, false).await;
    sqlx::query(
        "UPDATE worker_hosts SET max_concurrent = $2, advertised_max_concurrent = $3 WHERE id = $1",
    )
    .bind(host_a)
    .bind(2)
    .bind(2)
    .execute(store.pool())
    .await
    .expect("cap A 2");
    sqlx::query(
        "UPDATE worker_hosts SET max_concurrent = $2, advertised_max_concurrent = $3 WHERE id = $1",
    )
    .bind(host_b)
    .bind(8)
    .bind(8)
    .execute(store.pool())
    .await
    .expect("cap B 8");
    // A stays newer / listed first. Raw n would pick A (1 < 3); n*other_cap picks B (3/8 < 1/2).
    age_last_seen(&store, host_b, 20).await;

    let tasks = materialize_ready_tasks(&store, project.id, 4).await;
    enqueue_remote_on_host(&store, &tasks[0], host_a).await;
    for t in &tasks[1..] {
        enqueue_remote_on_host(&store, t, host_b).await;
    }

    let leftover_tm = leftover_cross_ws_this_machine(&store, project.id, &[host_a, host_b]).await;
    let chosen = select_assignable_host(&store, project.id)
        .await
        .expect("both under advertised cap");
    if leftover_tm.iter().any(|h| h.id == chosen.id()) {
        // Live fleet-host is merged into the picker; fixture ratio is not observable.
        return;
    }
    assert_eq!(
        chosen.id(),
        host_b,
        "1/2 listed first must lose to 3/8 (advertised load/cap, not raw in-flight count)"
    );
}

#[tokio::test]
async fn picker_equal_load_prefers_oldest_heartbeat() {
    let (store, _picker_lock) = setup().await;
    let project = store
        .create_project(&format!("pp-age-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    let older = pair_host(&store, project.id, "older", true, false).await;
    let newer = pair_host(&store, project.id, "newer", true, false).await;
    // Both 0/4 home. SQL lists last_seen DESC so newer is first; idx last
    // would also pick older unless we assert both stamps and From.
    age_last_seen(&store, older, 15).await;

    let listed = worker_hosts::list_assignable(
        store.pool(),
        project.id,
        fleet_db::WORKER_HOST_OFFLINE_AFTER,
    )
    .await
    .expect("list_assignable");
    let older_row = listed
        .iter()
        .find(|h| h.id == older)
        .expect("older must stay assignable");
    let newer_row = listed
        .iter()
        .find(|h| h.id == newer)
        .expect("newer must stay assignable");
    let older_seen = older_row
        .last_seen_at
        .expect("list_assignable requires last_seen");
    let newer_seen = newer_row
        .last_seen_at
        .expect("list_assignable requires last_seen");
    assert!(
        older_seen < newer_seen,
        "SQL must stamp older last_seen before newer ({older_seen} < {newer_seen})"
    );
    assert_eq!(
        AssignableHost::from(older_row).last_seen_at,
        Some(older_seen),
        "From must copy last_seen_at; dropping it still picks older via idx + newest-first SQL"
    );
    assert_eq!(
        AssignableHost::from(newer_row).last_seen_at,
        Some(newer_seen),
        "From must copy newer last_seen_at"
    );

    let chosen = select_assignable_host(&store, project.id)
        .await
        .expect("both online");
    assert_eq!(
        chosen.id(),
        older,
        "equal load: oldest heartbeat wins, not last_seen DESC"
    );
    assert_eq!(
        chosen.host.last_seen_at,
        Some(older_seen),
        "picked host must carry the older last_seen (not None via idx fallback)"
    );
}

#[tokio::test]
async fn container_min_isolation_skips_process_only_host() {
    let (store, _picker_lock) = setup().await;
    let project = store
        .create_project(&format!("pp-iso-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    workspaces::set_min_isolation(store.pool(), project.id, "container")
        .await
        .expect("min_isolation=container");
    let proc = pair_host(&store, project.id, "proc-only", true, false).await;

    let listed = worker_hosts::list_assignable(
        store.pool(),
        project.id,
        fleet_db::WORKER_HOST_OFFLINE_AFTER,
    )
    .await
    .expect("list_assignable");
    let proc_row = listed
        .iter()
        .find(|h| h.id == proc)
        .expect("process-only fixture must stay in list_assignable (empty-list is not this skip)");
    assert!(
        proc_row.advertised_host && !proc_row.advertised_container,
        "fixture must be process-only on the row the picker sees"
    );
    let home_only_proc = listed
        .iter()
        .filter(|h| h.workspace_id == project.id)
        .all(|h| h.id == proc);
    let any_container = listed.iter().any(|h| h.effective_container());
    // Picker also merges this-machine boxes from other workspaces (live
    // fleet-host on the shared DB). Those are not a process-only leftover.
    let leftover_tm_container = leftover_this_machine_container(&store, project.id, &[proc]).await;

    let chosen = select_assignable_host(&store, project.id).await;
    if home_only_proc && !any_container && !leftover_tm_container {
        assert!(
            chosen.is_none(),
            "container need with only a process-only assignable host must return None, not that host or a process leftover: {chosen:?}"
        );
    }
    if let Some(c) = chosen.as_ref() {
        assert_ne!(
            c.id(),
            proc,
            "a process-only host must not take a workspace that requires container isolation: {c:?}"
        );
        assert_eq!(
            c.runtime.as_str(),
            "container",
            "if a leftover cluster host wins, it must be container runtime, not process: {c:?}"
        );
    }
}

#[tokio::test]
async fn container_min_isolation_picks_container_host_not_newer_process_only() {
    let (store, _picker_lock) = setup().await;
    let project = store
        .create_project(&format!("pp-iso-pick-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    workspaces::set_min_isolation(store.pool(), project.id, "container")
        .await
        .expect("min_isolation=container");
    let ctr = pair_host(&store, project.id, "ctr", false, true).await;
    let proc = pair_host(&store, project.id, "proc-only", true, false).await;
    // Proc stays newer / listed first. Skip-all stays green on the singleton skip.
    age_last_seen(&store, ctr, 20).await;

    let listed = worker_hosts::list_assignable(
        store.pool(),
        project.id,
        fleet_db::WORKER_HOST_OFFLINE_AFTER,
    )
    .await
    .expect("list_assignable");
    let proc_row = listed
        .iter()
        .find(|h| h.id == proc)
        .expect("process-only must stay assignable");
    let ctr_row = listed
        .iter()
        .find(|h| h.id == ctr)
        .expect("container host must stay assignable");
    let proc_seen = proc_row
        .last_seen_at
        .expect("list_assignable requires last_seen");
    let ctr_seen = ctr_row
        .last_seen_at
        .expect("list_assignable requires last_seen");
    assert!(
        proc_seen > ctr_seen,
        "process-only must be newer / listed first ({proc_seen} > {ctr_seen})"
    );

    let chosen = select_assignable_host(&store, project.id)
        .await
        .expect("container host must meet container need (skip-all must not stay green)");
    assert_eq!(
        chosen.id(),
        ctr,
        "newer process-only listed first must lose to the container-capable host"
    );
    assert_eq!(
        chosen.runtime.as_str(),
        "container",
        "picked runtime must be container, not process"
    );
}

#[tokio::test]
async fn container_min_isolation_home_process_only_does_not_beat_cluster_container() {
    let (store, _picker_lock) = setup().await;
    let project = store
        .create_project(&format!("pp-iso-home-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    workspaces::set_min_isolation(store.pool(), project.id, "container")
        .await
        .expect("min_isolation=container");
    let other = store
        .create_project(&format!("pp-iso-cluster-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, other.id).await;
    let home_proc = pair_host(&store, project.id, "home-proc", true, false).await;
    let cluster_ctr = pair_host(&store, other.id, "cluster-ctr", false, true).await;
    sqlx::query(
        "UPDATE worker_hosts SET labels = array['cluster'], run_mode = 'in_container' WHERE id = $1",
    )
        .bind(cluster_ctr)
        .execute(store.pool())
        .await
        .expect("stamp cluster");
    // Both 0/4. Prefer-workspace would pick home_proc if isolation did not filter.

    let listed = worker_hosts::list_assignable(
        store.pool(),
        project.id,
        fleet_db::WORKER_HOST_OFFLINE_AFTER,
    )
    .await
    .expect("list_assignable");
    assert!(
        listed.iter().any(|h| h.id == home_proc),
        "home process-only must stay in the candidate set"
    );
    assert!(
        listed.iter().any(|h| h.id == cluster_ctr),
        "cluster container host must be visible to the home workspace"
    );

    let chosen = select_assignable_host(&store, project.id)
        .await
        .expect("cluster container host must meet container need");
    assert_eq!(
        chosen.id(),
        cluster_ctr,
        "prefer_workspace must not outrank isolation: home process-only cannot beat cluster container"
    );
    assert_eq!(
        chosen.runtime.as_str(),
        "container",
        "cluster leftover winner must be container runtime"
    );
}

#[tokio::test]
async fn container_need_skips_when_plane_disallows_container() {
    let (store, _picker_lock) = setup().await;
    let project = store
        .create_project(&format!("pp-iso-allow-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    workspaces::set_min_isolation(store.pool(), project.id, "container")
        .await
        .expect("min_isolation=container");
    let host = pair_host(&store, project.id, "ctr-disallowed", true, true).await;
    sqlx::query("UPDATE worker_hosts SET allow_container = false WHERE id = $1")
        .bind(host)
        .execute(store.pool())
        .await
        .expect("plane disallow container");

    let listed = worker_hosts::list_assignable(
        store.pool(),
        project.id,
        fleet_db::WORKER_HOST_OFFLINE_AFTER,
    )
    .await
    .expect("list_assignable");
    let row = listed.iter().find(|h| h.id == host).expect(
        "allow_host still true so the row stays in list_assignable; offered_classes must drop Container",
    );
    assert!(
        row.advertised_container && !row.allow_container,
        "advertised_container stays on; plane allow_container is off"
    );
    let home_only_this = listed
        .iter()
        .filter(|h| h.workspace_id == project.id)
        .all(|h| h.id == host);
    let any_container = listed.iter().any(|h| h.effective_container());
    let leftover_tm_container = leftover_this_machine_container(&store, project.id, &[host]).await;

    let chosen = select_assignable_host(&store, project.id).await;
    if home_only_this && !any_container && !leftover_tm_container {
        assert!(
            chosen.is_none(),
            "advertised_container with allow_container=false must skip (offered_classes ∩ allow): {chosen:?}"
        );
    }
    if let Some(c) = chosen.as_ref() {
        assert_ne!(
            c.id(),
            host,
            "plane-disallowed container host must not win: {c:?}"
        );
        assert_eq!(
            c.runtime.as_str(),
            "container",
            "if a leftover cluster host wins, it must be container runtime: {c:?}"
        );
    }
}

#[tokio::test]
async fn vm_min_isolation_skips_container_host() {
    let (store, _picker_lock) = setup().await;
    let project = store
        .create_project(&format!("pp-vm-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    workspaces::set_min_isolation(store.pool(), project.id, "vm")
        .await
        .expect("min_isolation=vm");
    let ctr = pair_host(&store, project.id, "ctr", false, true).await;

    let listed = worker_hosts::list_assignable(
        store.pool(),
        project.id,
        fleet_db::WORKER_HOST_OFFLINE_AFTER,
    )
    .await
    .expect("list_assignable");
    let ctr_row = listed
        .iter()
        .find(|h| h.id == ctr)
        .expect("container-only fixture must stay in list_assignable (empty-list is not this skip)");
    assert!(
        !ctr_row.advertised_host && ctr_row.advertised_container,
        "fixture must be container-only on the row the picker sees"
    );

    let chosen = select_assignable_host(&store, project.id).await;
    assert!(
        chosen.is_none(),
        "min_isolation=vm must return None: offered_classes never yields Vm, so a listed container host or leftover cluster winner must not be chosen: {chosen:?}"
    );
}

#[tokio::test]
async fn tick_vm_min_isolation_does_not_enqueue_container_host() {
    let (store, _picker_lock) = setup().await;
    let project = store
        .create_project(&format!("pp-vm-tick-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    workspaces::set_min_isolation(store.pool(), project.id, "vm")
        .await
        .expect("min_isolation=vm");
    let ctr = pair_host(&store, project.id, "ctr", false, true).await;

    let listed = worker_hosts::list_assignable(
        store.pool(),
        project.id,
        fleet_db::WORKER_HOST_OFFLINE_AFTER,
    )
    .await
    .expect("list_assignable");
    assert!(
        listed.iter().any(|h| h.id == ctr),
        "container-only fixture must stay in list_assignable before tick (empty-list is not this skip)"
    );

    store
        .insert_accepted_plan(project.id, one_task_plan(project.id))
        .await
        .unwrap();

    // Non-loopback so a broken vm-need cannot hide behind the container-guest
    // loopback refuse in spawn.rs.
    let r = runner(store.clone(), "http://api.example.test:8080")
        .tick_project(project.id)
        .await
        .unwrap();
    let claims = spawn_claim_count(&store, project.id).await;
    let ctr_claims = spawn_claim_count_for_host(&store, project.id, ctr).await;
    assert_eq!(
        r.spawned, 0,
        "tick must not enqueue when workspace_need is vm and no host offers Vm: {r:?}"
    );
    assert_eq!(
        claims, 0,
        "must not write a spawn_session claim under min_isolation=vm: {r:?}"
    );
    assert_eq!(
        ctr_claims, 0,
        "container-only host {ctr} must not receive a spawn_session claim: {r:?}"
    );
}

#[tokio::test]
async fn select_assignable_runner_preferring_vm_need_is_none() {
    let (store, _picker_lock) = setup().await;
    let project = store
        .create_project(&format!("pp-vm-pref-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    workspaces::set_min_isolation(store.pool(), project.id, "vm")
        .await
        .expect("min_isolation=vm");
    let ctr = pair_host(&store, project.id, "ctr", false, true).await;

    let listed = worker_hosts::list_assignable(
        store.pool(),
        project.id,
        fleet_db::WORKER_HOST_OFFLINE_AFTER,
    )
    .await
    .expect("list_assignable");
    assert!(
        listed.iter().any(|h| h.id == ctr),
        "container-only fixture must stay in list_assignable (empty-list is not this skip)"
    );

    let chosen = select_assignable_runner_preferring(
        &store,
        project.id,
        None,
        "http://api.example.test:8080",
    )
    .await;
    assert!(
        chosen.is_none(),
        "production picker must be None on a non-loopback plane when need is vm and the listed host is container-only: {chosen:?}"
    );
}

#[tokio::test]
async fn select_assignable_host_process_need_assigns_container_only() {
    let (store, _picker_lock) = setup().await;
    let project = store
        .create_project(&format!("pp-ctr-only-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    let host = pair_host(&store, project.id, "ctr-only", false, true).await;

    let chosen = select_assignable_runner_preferring(
        &store,
        project.id,
        None,
        "http://example.test:8080",
    )
    .await;
    assert!(
        chosen.is_none(),
        "on-pc picker must not assign a container-only host: {chosen:?}"
    );
    let _ = host;
}

#[tokio::test]
async fn tick_must_not_enqueue_container_work_to_a_loopback_plane() {
    let (store, _picker_lock) = setup().await;
    let project = store
        .create_project(&format!("pp-lb-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    workspaces::set_min_isolation(store.pool(), project.id, "container")
        .await
        .unwrap();
    let _ = pair_host(&store, project.id, "ctr", false, true).await;
    store
        .insert_accepted_plan(project.id, one_task_plan(project.id))
        .await
        .unwrap();

    let r = runner(store.clone(), "http://127.0.0.1:8080")
        .tick_project(project.id)
        .await
        .unwrap();
    let claims: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM work_claims WHERE workspace_id = $1 AND kind = 'spawn_session'",
    )
    .bind(project.id)
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(
        r.spawned, 0,
        "loopback plane URL is unreachable from a container guest: {r:?}"
    );
    assert_eq!(
        claims, 0,
        "must not enqueue a claim the guest cannot heartbeat"
    );
}

async fn spawn_claim_count(store: &Store, workspace_id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM work_claims WHERE workspace_id = $1 AND kind = 'spawn_session'",
    )
    .bind(workspace_id)
    .fetch_one(store.pool())
    .await
    .unwrap()
}

async fn spawn_claim_count_for_host(store: &Store, workspace_id: Uuid, host_id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM work_claims \
         WHERE workspace_id = $1 AND kind = 'spawn_session' AND payload->>'host_id' = $2",
    )
    .bind(workspace_id)
    .bind(host_id.to_string())
    .fetch_one(store.pool())
    .await
    .unwrap()
}

/// Documented follow-up: fail-closed assign when the plane URL
/// (`WORKER_PUBLIC_BASE` / orchestrator URL) is loopback and the paired
/// host is not this machine. Off-box workers cannot heartbeat to
/// 127/8, localhost, or [::1]. Local fleet-dev (this-machine + loopback)
/// may still assign. One loopback URL here; url_is_loopback already
/// classifies localhost / [::1] / 127/8.
#[tokio::test]
async fn tick_must_not_assign_offbox_host_when_plane_url_is_loopback() {
    let (store, _picker_lock) = setup().await;
    for url in [
        "http://127.0.0.1:8080",
        "http://localhost:8080",
        "http://[::1]:8080",
        "http://127.1.2.3:8080",
    ] {
        let project = store
            .create_project(&format!("pp-lb-off-{}", Uuid::new_v4()), 4)
            .await
            .unwrap();
        seed_workspace(&store, project.id).await;
        let off = pair_host(&store, project.id, "off-box-remote", true, false).await;
        let off_row = worker_hosts::get(store.pool(), off)
            .await
            .expect("get off-box")
            .expect("off-box");
        assert!(
            !host_is_this_machine(&AssignableHost::from(&off_row)),
            "off-box fixture must not match this machine"
        );

        store
            .insert_accepted_plan(project.id, one_task_plan(project.id))
            .await
            .unwrap();

        let r = runner(store.clone(), url)
            .tick_project(project.id)
            .await
            .unwrap();
        let off_claims = spawn_claim_count_for_host(&store, project.id, off).await;
        assert_eq!(
            off_claims, 0,
            "must not enqueue onto off-box {off} while plane URL {url} is loopback: {r:?}"
        );
        // A this-machine extra (the live CP laptop) may still take the claim.
        // That box can heartbeat to loopback. The off-box fixture cannot.
    }
}

/// Same-box loopback is local `fleet-dev`: a host whose hostname is this
/// machine may still take work when the plane URL is loopback.
#[tokio::test]
async fn tick_may_assign_this_machine_host_when_plane_url_is_loopback() {
    let (store, _picker_lock) = setup().await;
    let project = store
        .create_project(&format!("pp-lb-local-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    let local_name = require_this_machine_hostname();
    let local = pair_host(&store, project.id, &local_name, true, false).await;
    store
        .insert_accepted_plan(project.id, one_task_plan(project.id))
        .await
        .unwrap();

    let r = runner(store.clone(), "http://127.0.0.1:8080")
        .tick_project(project.id)
        .await
        .unwrap();
    let claims = spawn_claim_count(&store, project.id).await;
    assert!(
        r.spawned >= 1 || claims >= 1,
        "this-machine host {local} ({local_name}) must still be assignable \
         when the plane URL is loopback (local fleet-dev): {r:?} claims={claims}"
    );
}

/// Documented follow-up: fail-closed assign when the plane URL
/// (`WORKER_PUBLIC_BASE` / orchestrator URL) is loopback and the paired
/// host is not this machine. One loopback URL here; url_is_loopback already
/// classifies localhost / [::1] / 127/8. Off-box must stay in
/// list_assignable before tick so an empty list is not this skip.
#[tokio::test]
async fn tick_must_not_assign_listed_offbox_host_when_plane_url_is_loopback() {
    let (store, _picker_lock) = setup().await;
    let project = store
        .create_project(&format!("pp-lb-off-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    let off = pair_host(&store, project.id, "off-box-remote", true, false).await;
    let off_row = worker_hosts::get(store.pool(), off)
        .await
        .expect("get off-box")
        .expect("off-box");
    assert!(
        !host_is_this_machine(&AssignableHost::from(&off_row)),
        "off-box fixture must not match this machine"
    );

    let assignable = worker_hosts::list_assignable(
        store.pool(),
        project.id,
        fleet_db::WORKER_HOST_OFFLINE_AFTER,
    )
    .await
    .expect("list_assignable");
    assert!(
        assignable.iter().any(|h| h.id == off),
        "off-box fixture must stay in list_assignable before tick (empty-list is not this skip)"
    );

    store
        .insert_accepted_plan(project.id, one_task_plan(project.id))
        .await
        .unwrap();

    // Always tick. A leftover cluster this-machine may take the claim;
    // unfiltered pick would still prefer this home off-box.
    let r = runner(store.clone(), "http://127.0.0.1:8080")
        .tick_project(project.id)
        .await
        .unwrap();
    let off_claims = spawn_claim_count_for_host(&store, project.id, off).await;
    assert_eq!(
        off_claims, 0,
        "must not enqueue onto off-box {off} while plane URL is loopback: {r:?}"
    );
}

/// preferring is applied after host_reachable_on_plane. A preferred
/// off-box host must not restore an empty reachable set on loopback.
#[tokio::test]
async fn tick_must_not_honor_preferred_offbox_host_when_plane_url_is_loopback() {
    let (store, _picker_lock) = setup().await;
    let project = store
        .create_project(&format!("pp-lb-pref-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    let off = pair_host(&store, project.id, "off-box-preferred", true, false).await;
    let off_row = worker_hosts::get(store.pool(), off)
        .await
        .expect("get preferred off-box")
        .expect("preferred off-box");
    assert!(
        !host_is_this_machine(&AssignableHost::from(&off_row)),
        "preferred off-box fixture must not match this machine"
    );

    let mut plan = one_task_plan(project.id);
    plan.policy.preferred_host_id = Some(off);
    store.insert_accepted_plan(project.id, plan).await.unwrap();

    let r = runner(store.clone(), "http://127.0.0.1:8080")
        .tick_project(project.id)
        .await
        .unwrap();
    let off_claims = spawn_claim_count_for_host(&store, project.id, off).await;
    assert_eq!(
        off_claims, 0,
        "preferred off-box {off} must stay filtered on loopback: {r:?}"
    );
    assert_eq!(
        r.spawned, 0,
        "preferring must not restore an empty reachable set on loopback: {r:?}"
    );
}

/// Unfiltered pick prefers home off-box over a cluster this-machine.
/// Filter-before-pick must not fall back to that home winner on loopback.
#[tokio::test]
async fn tick_must_not_assign_home_offbox_over_cluster_this_machine_on_loopback() {
    let (store, _picker_lock) = setup().await;
    let project = store
        .create_project(&format!("pp-lb-home-off-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    let other = store
        .create_project(&format!("pp-lb-cluster-local-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, other.id).await;

    let local_name = std::env::var("HOSTNAME")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| {
            panic!("cluster this-machine fixture needs HOSTNAME or /etc/hostname")
        });
    let cluster_local = pair_host(&store, other.id, &local_name, true, false).await;
    sqlx::query(
        "UPDATE worker_hosts SET labels = array['cluster'], run_mode = 'in_container' WHERE id = $1",
    )
        .bind(cluster_local)
        .execute(store.pool())
        .await
        .expect("stamp cluster");
    let off = pair_host(&store, project.id, "off-box-home", true, false).await;

    let listed = worker_hosts::list_assignable(
        store.pool(),
        project.id,
        fleet_db::WORKER_HOST_OFFLINE_AFTER,
    )
    .await
    .expect("list_assignable");
    assert!(
        listed.iter().any(|h| h.id == off),
        "home off-box must stay in the candidate set"
    );
    assert!(
        listed.iter().any(|h| h.id == cluster_local),
        "cluster this-machine must be visible to the home workspace"
    );

    let unfiltered = select_assignable_host(&store, project.id)
        .await
        .expect("unfiltered pick must have a winner");
    assert_eq!(
        unfiltered.id(),
        off,
        "unfiltered pick must be home off-box, not cluster this-machine"
    );

    store
        .insert_accepted_plan(project.id, one_task_plan(project.id))
        .await
        .unwrap();

    let r = runner(store.clone(), "http://127.0.0.1:8080")
        .tick_project(project.id)
        .await
        .unwrap();
    let off_claims = spawn_claim_count_for_host(&store, project.id, off).await;
    assert_eq!(
        off_claims, 0,
        "must not enqueue onto home off-box {off} on loopback when cluster this-machine is assignable: {r:?}"
    );
}

/// Mixed pool: age off-box so unfiltered pick is off. Filter before pick
/// so the local box still gets the claim. Fail-closed if a leftover
/// cluster this-machine is already assignable (list_assignable OR cluster).
#[tokio::test]
async fn tick_loopback_assigns_this_machine_not_offbox() {
    let (store, _picker_lock) = setup().await;
    let project = store
        .create_project(&format!("pp-lb-mix-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    let off = pair_host(&store, project.id, "off-box-remote", true, false).await;
    let local_name = require_this_machine_hostname();
    let local = pair_host(&store, project.id, &local_name, true, false).await;
    age_last_seen(&store, off, 20).await;

    let off_row = worker_hosts::get(store.pool(), off)
        .await
        .expect("get off-box")
        .expect("off-box");
    assert!(
        !host_is_this_machine(&AssignableHost::from(&off_row)),
        "off-box fixture must not match this machine"
    );
    let local_row = worker_hosts::get(store.pool(), local)
        .await
        .expect("get local")
        .expect("local");
    assert!(
        host_is_this_machine(&AssignableHost::from(&local_row)),
        "local fixture {local_name} must match this machine"
    );

    let leftover = leftover_this_machine_ids(&store, project.id, &[local]).await;
    assert!(
        leftover.is_empty(),
        "leftover cluster this-machine {leftover:?} would steal the mixed-pool pick"
    );

    let unfiltered = select_assignable_host(&store, project.id)
        .await
        .expect("unfiltered pick must have a winner");
    assert_eq!(
        unfiltered.id(),
        off,
        "unfiltered pick must be older off-box, not this-machine"
    );

    store
        .insert_accepted_plan(project.id, one_task_plan(project.id))
        .await
        .unwrap();

    let r = runner(store.clone(), "http://127.0.0.1:8080")
        .tick_project(project.id)
        .await
        .unwrap();
    let off_claims = spawn_claim_count_for_host(&store, project.id, off).await;
    let local_claims = spawn_claim_count_for_host(&store, project.id, local).await;
    assert_eq!(
        off_claims, 0,
        "must not enqueue onto off-box {off} on loopback: {r:?}"
    );
    assert!(
        r.spawned >= 1 && local_claims >= 1,
        "this-machine {local} ({local_name}) must take the claim, not stay ready: \
         {r:?} local_claims={local_claims}"
    );
}

#[tokio::test]
async fn ollama_plan_sets_guest_harness_even_when_plane_env_is_mock() {
    let (store, _picker_lock) = setup().await;
    let project = store
        .create_project(&format!("pp-harness-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    let _ = pair_host(&store, project.id, "box", true, false).await;
    let mut plan = one_task_plan(project.id);
    plan.policy.llm_provider = Some("ollama".into());
    plan.policy.llm_model = Some(fleet_driver::mock_ollama::TEST_MODEL.into());
    store.insert_accepted_plan(project.id, plan).await.unwrap();

    unsafe {
        std::env::set_var("FLEET_HARNESS", "mock");
    }
    let r = runner(store.clone(), "http://api.example.test:8080")
        .tick_project(project.id)
        .await
        .unwrap();
    assert!(
        r.spawned >= 1,
        "expected a remote enqueue so we can inspect the claim env: {r:?}"
    );

    let payload: serde_json::Value = sqlx::query_scalar(
        "SELECT payload FROM work_claims WHERE workspace_id = $1 AND kind = 'spawn_session' \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(project.id)
    .fetch_one(store.pool())
    .await
    .expect("spawn claim");
    let harness = payload
        .get("env")
        .and_then(|e| e.get("FLEET_HARNESS"))
        .and_then(|v| v.as_str());
    assert_eq!(
        harness,
        Some("ollama"),
        "plan LLM is ollama; plane FLEET_HARNESS=mock must not win: {payload}"
    );
    assert_eq!(
        payload
            .get("env")
            .and_then(|e| e.get("FLEET_MODEL"))
            .and_then(|v| v.as_str()),
        Some(fleet_driver::mock_ollama::TEST_MODEL),
        "must carry the fixture model, not a cloud SKU: {payload}"
    );
    let files = payload.get("workspace_files").and_then(|v| v.as_object());
    assert!(
        files.is_some_and(|f| f.contains_key("fleet-tool.sh")),
        "spawn claim must pack fleet-tool.sh: {payload}"
    );
}

/// preferring is applied after host_reachable_on_plane. Preferred off-box
/// must not restore the unfiltered winner when this-machine is reachable.
#[tokio::test]
async fn tick_loopback_preferred_offbox_still_assigns_this_machine() {
    let (store, _picker_lock) = setup().await;
    let project = store
        .create_project(&format!("pp-lb-mix-pref-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    let off = pair_host(&store, project.id, "off-box-remote", true, false).await;
    let local_name = require_this_machine_hostname();
    let local = pair_host(&store, project.id, &local_name, true, false).await;
    age_last_seen(&store, off, 20).await;

    let leftover = leftover_this_machine_ids(&store, project.id, &[local]).await;
    assert!(
        leftover.is_empty(),
        "leftover cluster this-machine {leftover:?} would steal the mixed-pool pick"
    );

    let unfiltered = select_assignable_host(&store, project.id)
        .await
        .expect("unfiltered pick must have a winner");
    assert_eq!(
        unfiltered.id(),
        off,
        "unfiltered pick must be older off-box, not this-machine"
    );

    let mut plan = one_task_plan(project.id);
    plan.policy.preferred_host_id = Some(off);
    store.insert_accepted_plan(project.id, plan).await.unwrap();

    let r = runner(store.clone(), "http://127.0.0.1:8080")
        .tick_project(project.id)
        .await
        .unwrap();
    let off_claims = spawn_claim_count_for_host(&store, project.id, off).await;
    let local_claims = spawn_claim_count_for_host(&store, project.id, local).await;
    assert_eq!(
        off_claims, 0,
        "preferred off-box {off} must stay filtered on loopback: {r:?}"
    );
    assert!(
        r.spawned >= 1 && local_claims >= 1,
        "this-machine {local} must take the claim after preferred off-box is filtered: \
         {r:?} local_claims={local_claims}"
    );
}

/// host_is_this_machine IP arm: off-box-looking hostname + advertised
/// local non-loopback IP still takes the loopback claim.
#[tokio::test]
async fn tick_loopback_assigns_this_machine_by_advertised_ip() {
    let (store, _picker_lock) = setup().await;
    let project = store
        .create_project(&format!("pp-lb-mix-ip-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    let off = pair_host(&store, project.id, "off-box-remote", true, false).await;
    let local = pair_host(&store, project.id, "off-box-looking", true, false).await;
    let ip = require_local_nonloopback_ip();
    stamp_hardware_addrs(&store, local, &[&ip]).await;
    age_last_seen(&store, off, 20).await;

    let local_row = worker_hosts::get(store.pool(), local)
        .await
        .expect("get advertised-IP local")
        .expect("advertised-IP local");
    assert_eq!(
        local_row.hostname.as_deref(),
        Some("off-box-looking"),
        "fixture hostname must not be this machine"
    );
    assert!(
        host_is_this_machine(&AssignableHost::from(&local_row)),
        "advertised IP {ip} must match this machine"
    );

    let leftover = leftover_this_machine_ids(&store, project.id, &[local]).await;
    assert!(
        leftover.is_empty(),
        "leftover cluster this-machine {leftover:?} would steal the mixed-pool pick"
    );

    let unfiltered = select_assignable_host(&store, project.id)
        .await
        .expect("unfiltered pick must have a winner");
    assert_eq!(
        unfiltered.id(),
        off,
        "unfiltered pick must be older off-box, not advertised-IP this-machine"
    );

    store
        .insert_accepted_plan(project.id, one_task_plan(project.id))
        .await
        .unwrap();

    let r = runner(store.clone(), "http://127.0.0.1:8080")
        .tick_project(project.id)
        .await
        .unwrap();
    let off_claims = spawn_claim_count_for_host(&store, project.id, off).await;
    let local_claims = spawn_claim_count_for_host(&store, project.id, local).await;
    assert_eq!(
        off_claims, 0,
        "must not enqueue onto off-box {off} on loopback: {r:?}"
    );
    assert!(
        r.spawned >= 1 && local_claims >= 1,
        "advertised-IP this-machine {local} ({ip}) must take the claim: \
         {r:?} local_claims={local_claims}"
    );
}

/// Nameless Host+loopback is not this machine. Mixed with off-box, tick
/// must write no spawn_session (empty identity does not panic before tick).
#[tokio::test]
async fn tick_loopback_mixed_pool_refuses_when_this_machine_identity_is_empty() {
    let (store, _picker_lock) = setup().await;
    let project = store
        .create_project(&format!("pp-lb-mix-empty-{}", Uuid::new_v4()), 4)
        .await
        .unwrap();
    seed_workspace(&store, project.id).await;
    let off = pair_host(&store, project.id, "off-box-remote", true, false).await;
    let nameless = pair_host(&store, project.id, "nameless-host", true, false).await;
    clear_host_identity(&store, nameless).await;

    let nameless_row = worker_hosts::get(store.pool(), nameless)
        .await
        .expect("get nameless")
        .expect("nameless");
    assert!(
        nameless_row
            .hostname
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_none(),
        "nameless fixture must have empty hostname"
    );
    assert!(
        !host_is_this_machine(&AssignableHost::from(&nameless_row)),
        "hostname None + addrs [] is not this machine"
    );
    let off_row = worker_hosts::get(store.pool(), off)
        .await
        .expect("get off-box")
        .expect("off-box");
    assert!(
        !host_is_this_machine(&AssignableHost::from(&off_row)),
        "off-box fixture must not match this machine"
    );

    let leftover_home = leftover_this_machine_ids(&store, project.id, &[]).await;
    assert!(
        leftover_home.is_empty(),
        "leftover cluster this-machine {leftover_home:?} would take a claim this empty-identity pool must refuse"
    );
    let leftover_tm = leftover_cross_ws_this_machine(&store, project.id, &[]).await;

    store
        .insert_accepted_plan(project.id, one_task_plan(project.id))
        .await
        .unwrap();

    let r = runner(store.clone(), "http://127.0.0.1:8080")
        .tick_project(project.id)
        .await
        .unwrap();
    let claims = spawn_claim_count(&store, project.id).await;
    if leftover_tm.is_empty() {
        assert_eq!(
            r.spawned, 0,
            "nameless Host+loopback plus off-box must not assign: {r:?}"
        );
        assert_eq!(
            claims, 0,
            "must not write a spawn_session when no host is this machine: {r:?}"
        );
    } else {
        assert_eq!(
            spawn_claim_count_for_host(&store, project.id, nameless).await,
            0,
            "nameless fixture must not take the claim: {r:?}"
        );
        assert_eq!(
            spawn_claim_count_for_host(&store, project.id, off).await,
            0,
            "off-box fixture must not take the claim: {r:?}"
        );
    }
}
