//! fleet-driver binary talks heartbeat/status to a fake orch (no Postgres).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use fleet_driver::orch::OrchClient;
use serde::Deserialize;
use serde_json::Value;
use tempfile::tempdir;
use tokio::net::TcpListener;
use tokio::process::Command;

#[derive(Clone, Default)]
struct Rec {
    statuses: Vec<(i32, String, Option<String>, Option<String>)>,
    heartbeats: Vec<i32>,
    status_files: Vec<(String, Option<Value>)>,
}

#[derive(Deserialize)]
struct HeartbeatBody {
    seq: i32,
}

#[derive(Deserialize)]
struct StatusBody {
    seq: i32,
    status: String,
    reason: Option<String>,
    #[serde(default)]
    result_md: Option<String>,
    #[serde(default)]
    files: Option<Value>,
}

async fn heartbeat(
    State(rec): State<Arc<Mutex<Rec>>>,
    Json(body): Json<HeartbeatBody>,
) -> StatusCode {
    rec.lock().unwrap().heartbeats.push(body.seq);
    StatusCode::ACCEPTED
}

async fn status(State(rec): State<Arc<Mutex<Rec>>>, Json(body): Json<StatusBody>) -> StatusCode {
    let mut rec = rec.lock().unwrap();
    rec.status_files
        .push((body.status.clone(), body.files));
    rec.statuses
        .push((body.seq, body.status, body.reason, body.result_md));
    StatusCode::ACCEPTED
}

#[tokio::test]
async fn driver_binary_completes_mock_turn() {
    let rec = Arc::new(Mutex::new(Rec::default()));
    let app = Router::new()
        .route(
            "/v1/projects/{project_id}/workers/{session_id}/heartbeat",
            post(heartbeat),
        )
        .route(
            "/v1/projects/{project_id}/workers/{session_id}/status",
            post(status),
        )
        .with_state(rec.clone());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("spec.md"), "Say hello via mock agent.").unwrap();

    let driver = env!("CARGO_BIN_EXE_fleet-driver");
    let mock = env!("CARGO_BIN_EXE_mock-acp-agent");
    let project = uuid_like();
    let session = uuid_like();
    let task = uuid_like();

    let mut child = Command::new(driver)
        .env("FLEET_ORCHESTRATOR_URL", format!("http://{addr}"))
        .env("FLEET_PROJECT_ID", &project)
        .env("FLEET_SESSION_ID", &session)
        .env("FLEET_BOARD_TASK_ID", &task)
        .env("FLEET_WORKER_TOKEN", "tok")
        .env("FLEET_WORK_DIR", dir.path())
        .env("FLEET_HARNESS", "mock")
        .env("FLEET_MOCK_ACP_BIN", mock)
        .env("FLEET_TASK_TIMEOUT_S", "15")
        .env("FLEET_HEARTBEAT_INTERVAL_S", "30")
        .env("RUST_LOG", "warn")
        .kill_on_drop(true)
        .spawn()
        .expect("spawn fleet-driver");

    let status = tokio::time::timeout(Duration::from_secs(20), child.wait())
        .await
        .expect("driver timed out")
        .expect("wait");
    assert!(status.success(), "driver exit {status}");

    let transcript = std::fs::read_to_string(dir.path().join("artifacts/transcript.md")).unwrap();
    assert!(transcript.contains("hello from mock agent"), "{transcript}");
    let result: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("artifacts/result.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["harness"], "mock");

    let rec = rec.lock().unwrap();
    assert!(
        rec.heartbeats.contains(&2),
        "initial heartbeat seq=2 missing: {:?}",
        rec.heartbeats
    );
    assert!(
        rec.statuses.iter().any(|(_, s, _, _)| s == "running"),
        "statuses={:?}",
        rec.statuses
    );
    assert!(
        rec.statuses.iter().any(|(_, s, _, _)| s == "success"),
        "statuses={:?}",
        rec.statuses
    );
    assert!(
        rec.statuses.iter().any(|(_, s, _, md)| {
            s == "success"
                && md
                    .as_deref()
                    .is_some_and(|t| t.contains("hello from mock agent"))
        }),
        "success result_md must include the transcript: {:?}",
        rec.statuses
    );
}

#[tokio::test]
async fn driver_binary_ignores_legacy_fleet_work_cmd() {
    let rec = Arc::new(Mutex::new(Rec::default()));
    let addr = serve_rec_orch(rec).await;
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("spec.md"), "Say hello via mock agent.").unwrap();
    let marker = dir.path().join("from_work_cmd");

    let mut child = driver_cmd(addr, dir.path())
        .env("FLEET_WORK_CMD", format!("touch {}", marker.display()))
        .spawn()
        .expect("spawn fleet-driver");
    let status = wait_driver(&mut child).await;
    assert!(status.success(), "driver exit {status}");
    assert!(
        !marker.exists(),
        "fleet-driver must ignore leftover FLEET_WORK_CMD"
    );
}

#[tokio::test]
async fn driver_binary_posts_failed_and_exits_nonzero_when_mock_fails() {
    let rec = Arc::new(Mutex::new(Rec::default()));
    let addr = serve_rec_orch(rec.clone()).await;
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("spec.md"), "Say hello via mock agent.").unwrap();
    let mock = env!("CARGO_BIN_EXE_mock-acp-agent");

    let mut child = driver_cmd(addr, dir.path())
        .env("FLEET_HARNESS_CMD", format!("{mock} --fail"))
        .spawn()
        .expect("spawn fleet-driver");
    let status = wait_driver(&mut child).await;
    assert!(!status.success(), "failed mock turn must exit nonzero: {status}");

    let rec = rec.lock().unwrap();
    assert!(
        rec.statuses.iter().any(|(_, s, _, _)| s == "failed"),
        "statuses={:?}",
        rec.statuses
    );
}

#[tokio::test]
async fn driver_binary_exits_nonzero_when_plane_returns_401() {
    async fn always_unauthorized() -> StatusCode {
        StatusCode::UNAUTHORIZED
    }
    let app = Router::new()
        .route(
            "/v1/projects/{project_id}/workers/{session_id}/heartbeat",
            post(always_unauthorized),
        )
        .route(
            "/v1/projects/{project_id}/workers/{session_id}/status",
            post(always_unauthorized),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let dir = tempdir().unwrap();
    let mut child = driver_cmd(addr, dir.path())
        .spawn()
        .expect("spawn fleet-driver");
    let status = wait_driver(&mut child).await;
    assert_eq!(status.code(), Some(2), "plane 401 on first status must exit 2: {status}");
}

#[tokio::test]
async fn driver_binary_success_status_includes_work_keep_files() {
    let rec = Arc::new(Mutex::new(Rec::default()));
    let addr = serve_rec_orch(rec.clone()).await;
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("spec.md"), "Say hello via mock agent.").unwrap();
    // collect_guest_files packs existing allowlisted files; the driver does
    // not invent work/.keep. Plant it so success status files include it.
    std::fs::create_dir_all(dir.path().join("work")).unwrap();
    std::fs::write(dir.path().join("work/.keep"), "ok").unwrap();

    let mut child = driver_cmd(addr, dir.path())
        .spawn()
        .expect("spawn fleet-driver");
    let status = wait_driver(&mut child).await;
    assert!(status.success(), "driver exit {status}");

    let rec = rec.lock().unwrap();
    assert!(
        rec.status_files.iter().any(|(s, files)| {
            s == "success"
                && files.as_ref().is_some_and(|f| {
                    f.get("work/.keep") == Some(&Value::String("ok".into()))
                })
        }),
        "success files must include work/.keep: {:?}",
        rec.status_files
    );
}

/// OrchClient contract: send `Authorization: Bearer <session token>` and
/// map plane 401 to Err. This fixture always 401s — it does not implement
/// the worker-session gate. Product reject is fleet-orchestrator WorkerAuth
/// + `authenticate_worker_session` (see api_integration).
#[tokio::test]
async fn orch_client_sends_bearer_and_maps_401_to_err() {
    #[derive(Clone, Default)]
    struct Rec {
        auths: Arc<Mutex<Vec<String>>>,
    }

    async fn always_unauthorized(State(rec): State<Rec>, headers: HeaderMap) -> StatusCode {
        let got = headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        rec.auths.lock().unwrap().push(got);
        StatusCode::UNAUTHORIZED
    }

    let rec = Rec::default();
    let auths = rec.auths.clone();
    let app = Router::new()
        .route(
            "/v1/projects/{project_id}/workers/{session_id}/heartbeat",
            post(always_unauthorized),
        )
        .route(
            "/v1/projects/{project_id}/workers/{session_id}/status",
            post(always_unauthorized),
        )
        .with_state(rec);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let project = uuid_like();
    let session = uuid_like();
    let token = "session-worker-token";
    let client = OrchClient::new(format!("http://{addr}"), &project, &session, token).unwrap();

    let hb = client.post_heartbeat(1, None, &[]).await;
    assert!(hb.is_err(), "plane 401 must be Err: {hb:?}");
    let st = client.post_status(2, "running", None).await;
    assert!(st.is_err(), "plane 401 must be Err: {st:?}");

    let posted = auths.lock().unwrap();
    assert_eq!(
        posted.len(),
        2,
        "heartbeat and status must both hit the plane"
    );
    let expected = format!("Bearer {token}");
    assert!(
        posted.iter().all(|h| h == &expected),
        "OrchClient must send Authorization: Bearer <session token>: {posted:?}"
    );
}

async fn serve_rec_orch(rec: Arc<Mutex<Rec>>) -> SocketAddr {
    let app = Router::new()
        .route(
            "/v1/projects/{project_id}/workers/{session_id}/heartbeat",
            post(heartbeat),
        )
        .route(
            "/v1/projects/{project_id}/workers/{session_id}/status",
            post(status),
        )
        .with_state(rec);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    addr
}

fn driver_cmd(addr: SocketAddr, work_dir: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fleet-driver"));
    cmd.env("FLEET_ORCHESTRATOR_URL", format!("http://{addr}"))
        .env("FLEET_PROJECT_ID", uuid_like())
        .env("FLEET_SESSION_ID", uuid_like())
        .env("FLEET_BOARD_TASK_ID", uuid_like())
        .env("FLEET_WORKER_TOKEN", "tok")
        .env("FLEET_WORK_DIR", work_dir)
        .env("FLEET_HARNESS", "mock")
        .env("FLEET_MOCK_ACP_BIN", env!("CARGO_BIN_EXE_mock-acp-agent"))
        .env("FLEET_TASK_TIMEOUT_S", "15")
        .env("FLEET_HEARTBEAT_INTERVAL_S", "30")
        .env("RUST_LOG", "warn")
        .kill_on_drop(true);
    cmd
}

async fn wait_driver(child: &mut tokio::process::Child) -> std::process::ExitStatus {
    tokio::time::timeout(Duration::from_secs(20), child.wait())
        .await
        .expect("driver timed out")
        .expect("wait")
}

fn uuid_like() -> String {
    // Not a real UUID; orch fake server does not parse ids.
    format!("{:032x}", rand_u128())
}

fn rand_u128() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(1);
    t ^ (std::process::id() as u128) << 64
}
