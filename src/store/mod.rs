//! Postgres store layer (PR3).

mod models;
mod pool;

pub use models::*;
pub use pool::*;

use crate::domain::{
    stable_board_task_id, WorkPlanBody, WorkPlanStatus, WorkRunStatus,
};
use crate::validate::validate_plan;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Transaction};
use subtle::ConstantTimeEq;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error("validation: {0}")]
    Validation(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Clone)]
pub struct Store {
    pool: PgPool,
}

impl Store {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn create_project(&self, name: &str, max_agents: i32) -> Result<ProjectRow> {
        let mut tx = self.pool.begin().await?;
        let project = sqlx::query_as::<_, ProjectRow>(
            r#"
            INSERT INTO projects (name)
            VALUES ($1)
            RETURNING id, name, created_at
            "#,
        )
        .bind(name)
        .fetch_one(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO project_configs (project_id, max_agents)
            VALUES ($1, $2)
            "#,
        )
        .bind(project.id)
        .bind(max_agents)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(project)
    }

    pub async fn get_project_config(&self, project_id: Uuid) -> Result<ProjectConfigRow> {
        sqlx::query_as::<_, ProjectConfigRow>(
            r#"
            SELECT project_id, max_agents, tick_interval_s, heartbeat_stale_s,
                   spawn_timeout_s, kill_grace_s, default_task_timeout_s,
                   default_max_retries, retry_backoff_s, on_task_fail,
                   auto_sync_external, sandbox_backend, updated_at
            FROM project_configs
            WHERE project_id = $1
            "#,
        )
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| StoreError::NotFound(format!("project config {project_id}")))
    }

    pub async fn insert_accepted_plan(
        &self,
        project_id: Uuid,
        mut body: WorkPlanBody,
    ) -> Result<(WorkRunRow, WorkPlanRow)> {
        validate_plan(&body).map_err(|e| StoreError::Validation(e.to_string()))?;
        if body.project_id != project_id {
            return Err(StoreError::Validation(
                "body.project_id must match path project".into(),
            ));
        }
        body.status = WorkPlanStatus::Accepted;

        let mut tx = self.pool.begin().await?;
        let run = sqlx::query_as::<_, WorkRunRow>(
            r#"
            INSERT INTO work_runs (id, project_id, status, source)
            VALUES ($1, $2, $3, $4)
            RETURNING id, project_id, status, source, cancel_requested_at,
                      orchestration_owned_at, first_spawn_at, terminal_at,
                      created_at, updated_at
            "#,
        )
        .bind(body.run_id)
        .bind(project_id)
        .bind(WorkRunStatus::Accepted.as_db())
        .bind(&body.source)
        .fetch_one(&mut *tx)
        .await?;

        let body_json =
            serde_json::to_value(&body).map_err(|e| StoreError::Validation(e.to_string()))?;

        let plan = sqlx::query_as::<_, WorkPlanRow>(
            r#"
            INSERT INTO work_plans (
                id, project_id, run_id, version, status, source, body, accepted_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, now())
            RETURNING id, project_id, run_id, version, status, source, body,
                      accepted_at, materialized_at, in_force_at, created_at
            "#,
        )
        .bind(body.plan_id)
        .bind(project_id)
        .bind(body.run_id)
        .bind(body.version)
        .bind(WorkPlanStatus::Accepted.as_db())
        .bind(&body.source)
        .bind(body_json)
        .fetch_one(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok((run, plan))
    }

    pub async fn materialize_plan(&self, project_id: Uuid, plan_id: Uuid) -> Result<usize> {
        let mut tx = self.pool.begin().await?;
        let count = materialize_in_tx(&mut tx, project_id, plan_id).await?;
        tx.commit().await?;
        Ok(count)
    }

    pub async fn load_board_snapshot(&self, project_id: Uuid) -> Result<BoardSnapshot> {
        let tasks = sqlx::query_as::<_, BoardTaskRow>(
            r#"
            SELECT id, project_id, run_id, plan_id, plan_task_id, title, summary, spec_md,
                   status, depends_on, priority, worker_profile, acceptance, external_ref,
                   timeout_s, max_retries, attempt, claimed_session_id, last_report_seq,
                   progress_pct, block_reason, hitl_notes, hitl_artifacts,
                   hitl_resume_requested_at, hitl_notified_at, hitl_resolved_at,
                   failure_reason, artifacts, retry_not_before,
                   assigned_at, started_at, terminal_at, created_at, updated_at
            FROM board_tasks
            WHERE project_id = $1
            ORDER BY priority DESC, created_at ASC
            "#,
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;

        let sessions = sqlx::query_as::<_, WorkerSessionRow>(
            r#"
            SELECT id, project_id, run_id, board_task_id, attempt, status,
                   runtime_backend, runtime_handle, worker_token_hash, worker_profile,
                   last_heartbeat_at, progress_pct, exit_reason, kill_grace_deadline,
                   work_dir, started_at, exited_at, created_at
            FROM worker_sessions
            WHERE project_id = $1
            "#,
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(BoardSnapshot { tasks, sessions })
    }

    pub async fn request_cancel(&self, project_id: Uuid, run_id: Uuid) -> Result<()> {
        let res = sqlx::query(
            r#"
            UPDATE work_runs
            SET cancel_requested_at = COALESCE(cancel_requested_at, now()),
                updated_at = now()
            WHERE id = $1 AND project_id = $2
            "#,
        )
        .bind(run_id)
        .bind(project_id)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!("run {run_id}")));
        }
        Ok(())
    }

    pub async fn insert_session_spawning(
        &self,
        session: &NewWorkerSession,
    ) -> Result<WorkerSessionRow> {
        let token_hash = hash_worker_token(&session.worker_token);
        sqlx::query_as::<_, WorkerSessionRow>(
            r#"
            INSERT INTO worker_sessions (
                id, project_id, run_id, board_task_id, attempt, status,
                runtime_backend, worker_token_hash, worker_profile, work_dir, started_at
            )
            VALUES ($1,$2,$3,$4,$5,'spawning',$6,$7,$8,$9, now())
            RETURNING id, project_id, run_id, board_task_id, attempt, status,
                      runtime_backend, runtime_handle, worker_token_hash, worker_profile,
                      last_heartbeat_at, progress_pct, exit_reason, kill_grace_deadline,
                      work_dir, started_at, exited_at, created_at
            "#,
        )
        .bind(session.id)
        .bind(session.project_id)
        .bind(session.run_id)
        .bind(session.board_task_id)
        .bind(session.attempt)
        .bind(&session.runtime_backend)
        .bind(token_hash)
        .bind(&session.worker_profile)
        .bind(&session.work_dir)
        .fetch_one(&self.pool)
        .await
        .map_err(Into::into)
    }

    pub async fn append_worker_report(
        &self,
        project_id: Uuid,
        session_id: Uuid,
        board_task_id: Uuid,
        seq: i32,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO worker_reports (project_id, session_id, board_task_id, seq, kind, payload)
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (session_id, seq) DO NOTHING
            "#,
        )
        .bind(project_id)
        .bind(session_id)
        .bind(board_task_id)
        .bind(seq)
        .bind(kind)
        .bind(payload)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_unapplied_reports(&self, project_id: Uuid) -> Result<Vec<WorkerReportRow>> {
        sqlx::query_as::<_, WorkerReportRow>(
            r#"
            SELECT id, project_id, session_id, board_task_id, seq, kind, payload,
                   received_at, applied_at
            FROM worker_reports
            WHERE project_id = $1 AND applied_at IS NULL
            ORDER BY id ASC
            "#,
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
        .map_err(Into::into)
    }

    pub async fn cas_assign_task(
        &self,
        task_id: Uuid,
        session_id: Uuid,
        attempt: i32,
    ) -> Result<bool> {
        let res = sqlx::query(
            r#"
            UPDATE board_tasks
            SET status = 'assigned',
                claimed_session_id = $2,
                attempt = $3,
                assigned_at = now(),
                updated_at = now()
            WHERE id = $1 AND status = 'ready'
            "#,
        )
        .bind(task_id)
        .bind(session_id)
        .bind(attempt)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }
}

#[derive(sqlx::FromRow)]
struct PlanLockRow {
    #[allow(dead_code)]
    id: Uuid,
    body: serde_json::Value,
    materialized_at: Option<chrono::DateTime<chrono::Utc>>,
    run_id: Uuid,
    run_status: String,
    cancel_requested_at: Option<chrono::DateTime<chrono::Utc>>,
}

async fn materialize_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    project_id: Uuid,
    plan_id: Uuid,
) -> Result<usize> {
    let plan = sqlx::query_as::<_, PlanLockRow>(
        r#"
        SELECT p.id, p.body, p.materialized_at, p.run_id,
               r.status AS run_status, r.cancel_requested_at
        FROM work_plans p
        JOIN work_runs r ON r.id = p.run_id
        WHERE p.id = $1 AND p.project_id = $2
        FOR UPDATE OF p
        "#,
    )
    .bind(plan_id)
    .bind(project_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("plan {plan_id}")))?;

    if plan.materialized_at.is_some() {
        return Ok(0);
    }
    if plan.cancel_requested_at.is_some() || plan.run_status != WorkRunStatus::Accepted.as_db() {
        return Ok(0);
    }

    let body: WorkPlanBody = serde_json::from_value(plan.body)
        .map_err(|e| StoreError::Validation(format!("plan body: {e}")))?;
    validate_plan(&body).map_err(|e| StoreError::Validation(e.to_string()))?;

    let mut id_map = std::collections::HashMap::new();
    for t in &body.tasks {
        id_map.insert(
            t.plan_task_id.clone(),
            stable_board_task_id(project_id, plan_id, &t.plan_task_id),
        );
    }

    let mut count = 0usize;
    for t in &body.tasks {
        let board_id = id_map[&t.plan_task_id];
        let depends: Vec<Uuid> = t.depends_on.iter().map(|d| id_map[d]).collect();
        let acceptance = serde_json::to_value(&t.acceptance).unwrap_or(serde_json::json!([]));
        let external = t
            .external_ref
            .as_ref()
            .map(|e| serde_json::to_value(e).unwrap_or(serde_json::Value::Null));
        let max_retries = t.max_retries.unwrap_or(2);

        sqlx::query(
            r#"
            INSERT INTO board_tasks (
                id, project_id, run_id, plan_id, plan_task_id, title, summary, spec_md,
                status, depends_on, priority, worker_profile, acceptance, external_ref,
                timeout_s, max_retries
            )
            VALUES (
                $1,$2,$3,$4,$5,$6,$7,$8,
                'queued',$9,$10,$11,$12,$13,
                $14,$15
            )
            ON CONFLICT (project_id, plan_id, plan_task_id) DO NOTHING
            "#,
        )
        .bind(board_id)
        .bind(project_id)
        .bind(plan.run_id)
        .bind(plan_id)
        .bind(&t.plan_task_id)
        .bind(&t.title)
        .bind(&t.summary)
        .bind(&t.spec_md)
        .bind(&depends)
        .bind(t.priority)
        .bind(&t.worker_profile)
        .bind(acceptance)
        .bind(external)
        .bind(t.timeout_s)
        .bind(max_retries)
        .execute(&mut **tx)
        .await?;
        count += 1;
    }

    sqlx::query(
        r#"
        UPDATE work_plans
        SET materialized_at = now(), in_force_at = now()
        WHERE id = $1
        "#,
    )
    .bind(plan_id)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        r#"
        UPDATE work_runs
        SET status = $2,
            orchestration_owned_at = COALESCE(orchestration_owned_at, now()),
            updated_at = now()
        WHERE id = $1
        "#,
    )
    .bind(plan.run_id)
    .bind(WorkRunStatus::Active.as_db())
    .execute(&mut **tx)
    .await?;

    Ok(count)
}

pub fn hash_worker_token(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    hex::encode(h.finalize())
}

pub fn verify_worker_token(token: &str, hash_hex: &str) -> bool {
    let got = hash_worker_token(token);
    got.as_bytes().ct_eq(hash_hex.as_bytes()).into()
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn token_hash_roundtrip() {
        let h = hash_worker_token("secret-token");
        assert!(verify_worker_token("secret-token", &h));
        assert!(!verify_worker_token("wrong", &h));
    }
}
