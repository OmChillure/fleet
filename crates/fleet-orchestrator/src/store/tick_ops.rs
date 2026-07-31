//! Store operations used by the reconcile tick (PR5b/c).

use super::{hash_worker_token, Result, Store, WorkerSessionRow};
use chrono::{DateTime, Utc};
use uuid::Uuid;

impl Store {
    /// CAS ready→assigned, insert spawning session, set claim. Returns (attempt, session_id, token) or None if CAS lost.
    pub async fn begin_spawn_assignment(
        &self,
        task_id: Uuid,
        project_id: Uuid,
        run_id: Uuid,
        worker_profile: &str,
        runtime_backend: &str,
        work_dir: Option<&str>,
    ) -> Result<Option<SpawnAssignment>> {
        let mut tx = self.pool.begin().await?;

        let row = sqlx::query_as::<_, (i32,)>(
            r#"
            UPDATE board_tasks
            SET status = 'assigned',
                attempt = attempt + 1,
                assigned_at = now(),
                updated_at = now()
            WHERE id = $1 AND status = 'ready'
            RETURNING attempt
            "#,
        )
        .bind(task_id)
        .fetch_optional(&mut *tx)
        .await?;

        let Some((attempt,)) = row else {
            tx.commit().await?;
            return Ok(None);
        };

        let session_id = Uuid::new_v4();
        let token = Uuid::new_v4().to_string();
        let token_hash = hash_worker_token(&token);

        sqlx::query(
            r#"
            INSERT INTO worker_sessions (
                id, project_id, run_id, board_task_id, attempt, status,
                runtime_backend, worker_token_hash, worker_profile, work_dir, started_at
            )
            VALUES ($1,$2,$3,$4,$5,'spawning',$6,$7,$8,$9, now())
            "#,
        )
        .bind(session_id)
        .bind(project_id)
        .bind(run_id)
        .bind(task_id)
        .bind(attempt)
        .bind(runtime_backend)
        .bind(&token_hash)
        .bind(worker_profile)
        .bind(work_dir)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            UPDATE board_tasks
            SET claimed_session_id = $2, updated_at = now()
            WHERE id = $1
            "#,
        )
        .bind(task_id)
        .bind(session_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(Some(SpawnAssignment {
            session_id,
            attempt,
            worker_token: token,
        }))
    }

    pub async fn complete_spawn_success(
        &self,
        session_id: Uuid,
        task_id: Uuid,
        run_id: Uuid,
        runtime_handle: &str,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
            UPDATE worker_sessions
            SET runtime_handle = $2,
                status = 'healthy',
                last_heartbeat_at = now(),
                started_at = COALESCE(started_at, now())
            WHERE id = $1
            "#,
        )
        .bind(session_id)
        .bind(runtime_handle)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            UPDATE board_tasks
            SET status = 'in_progress',
                started_at = COALESCE(started_at, now()),
                updated_at = now()
            WHERE id = $1 AND status = 'assigned'
            "#,
        )
        .bind(task_id)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            UPDATE work_runs
            SET first_spawn_at = COALESCE(first_spawn_at, now()), updated_at = now()
            WHERE id = $1
            "#,
        )
        .bind(run_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    pub async fn complete_spawn_failure(&self, session_id: Uuid) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE worker_sessions
            SET status = 'failed_spawn',
                exit_reason = 'spawn_failed',
                exited_at = now()
            WHERE id = $1
            "#,
        )
        .bind(session_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn repair_assigned_to_in_progress(&self, project_id: Uuid) -> Result<u64> {
        let res = sqlx::query(
            r#"
            UPDATE board_tasks t
            SET status = 'in_progress',
                started_at = COALESCE(t.started_at, now()),
                updated_at = now()
            FROM worker_sessions s
            WHERE t.project_id = $1
              AND t.status = 'assigned'
              AND t.claimed_session_id = s.id
              AND s.status = 'healthy'
            "#,
        )
        .bind(project_id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    pub async fn mark_report_applied(&self, report_id: i64) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE worker_reports SET applied_at = now() WHERE id = $1 AND applied_at IS NULL
            "#,
        )
        .bind(report_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn apply_heartbeat(
        &self,
        session_id: Uuid,
        task_id: Uuid,
        seq: i32,
        progress_pct: Option<i32>,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE worker_sessions
            SET last_heartbeat_at = now(),
                progress_pct = COALESCE($2, progress_pct)
            WHERE id = $1
            "#,
        )
        .bind(session_id)
        .bind(progress_pct)
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            UPDATE board_tasks
            SET last_report_seq = GREATEST(last_report_seq, $2),
                progress_pct = COALESCE($3, progress_pct),
                updated_at = now()
            WHERE id = $1
            "#,
        )
        .bind(task_id)
        .bind(seq)
        .bind(progress_pct)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Terminal success/fail from worker report; clears claim (caller destroys runtime).
    pub async fn apply_task_terminal(
        &self,
        task_id: Uuid,
        session_id: Uuid,
        status: &str, // done_success | done_failed | blocked
        failure_or_block: Option<&str>,
        seq: i32,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        if status == "blocked" {
            sqlx::query(
                r#"
                UPDATE board_tasks
                SET status = 'blocked',
                    block_reason = $2,
                    last_report_seq = GREATEST(last_report_seq, $3),
                    updated_at = now()
                WHERE id = $1 AND status IN ('assigned', 'in_progress', 'blocked')
                "#,
            )
            .bind(task_id)
            .bind(failure_or_block)
            .bind(seq)
            .execute(&mut *tx)
            .await?;
        } else {
            sqlx::query(
                r#"
                UPDATE board_tasks
                SET status = $2,
                    failure_reason = CASE WHEN $2 = 'done_failed' THEN $3 ELSE failure_reason END,
                    last_report_seq = GREATEST(last_report_seq, $4),
                    terminal_at = now(),
                    claimed_session_id = NULL,
                    updated_at = now()
                WHERE id = $1 AND status NOT IN ('done_success', 'done_failed', 'cancelled')
                "#,
            )
            .bind(task_id)
            .bind(status)
            .bind(failure_or_block)
            .bind(seq)
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                r#"
                UPDATE worker_sessions
                SET status = 'exited',
                    exit_reason = CASE WHEN $2 = 'done_success' THEN 'clean' ELSE 'worker_failed' END,
                    exited_at = now()
                WHERE id = $1 AND status NOT IN ('exited', 'failed_spawn')
                "#,
            )
            .bind(session_id)
            .bind(status)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn free_session_claim(
        &self,
        task_id: Uuid,
        session_id: Uuid,
        exit_reason: &str,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
            UPDATE worker_sessions
            SET status = 'exited',
                exit_reason = $2,
                exited_at = COALESCE(exited_at, now())
            WHERE id = $1 AND status NOT IN ('exited', 'failed_spawn')
            "#,
        )
        .bind(session_id)
        .bind(exit_reason)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            UPDATE board_tasks
            SET claimed_session_id = NULL, updated_at = now()
            WHERE id = $1 AND claimed_session_id = $2
            "#,
        )
        .bind(task_id)
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn set_task_retry_wait(
        &self,
        task_id: Uuid,
        retry_not_before: DateTime<Utc>,
        failure_reason: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE board_tasks
            SET status = 'retry_wait',
                claimed_session_id = NULL,
                retry_not_before = $2,
                failure_reason = COALESCE($3, failure_reason),
                updated_at = now()
            WHERE id = $1 AND status NOT IN ('done_success', 'done_failed', 'cancelled')
            "#,
        )
        .bind(task_id)
        .bind(retry_not_before)
        .bind(failure_reason)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_task_cancelled(&self, task_id: Uuid, reason: &str) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE board_tasks
            SET status = 'cancelled',
                failure_reason = $2,
                claimed_session_id = NULL,
                terminal_at = now(),
                updated_at = now()
            WHERE id = $1 AND status NOT IN ('done_success', 'done_failed', 'cancelled')
            "#,
        )
        .bind(task_id)
        .bind(reason)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_task_done_failed(&self, task_id: Uuid, reason: &str) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE board_tasks
            SET status = 'done_failed',
                failure_reason = $2,
                claimed_session_id = NULL,
                terminal_at = now(),
                updated_at = now()
            WHERE id = $1 AND status NOT IN ('done_success', 'done_failed', 'cancelled')
            "#,
        )
        .bind(task_id)
        .bind(reason)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn promote_retry_wait(&self, project_id: Uuid, now: DateTime<Utc>) -> Result<u64> {
        let res = sqlx::query(
            r#"
            UPDATE board_tasks t
            SET status = 'ready',
                retry_not_before = NULL,
                updated_at = now()
            FROM work_runs r
            WHERE t.project_id = $1
              AND t.run_id = r.id
              AND t.status = 'retry_wait'
              AND (t.retry_not_before IS NULL OR t.retry_not_before <= $2)
              AND r.cancel_requested_at IS NULL
              AND r.status = 'active'
            "#,
        )
        .bind(project_id)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    pub async fn mark_session_killing(
        &self,
        session_id: Uuid,
        exit_reason: &str,
        grace_deadline: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE worker_sessions
            SET status = 'killing',
                exit_reason = $2,
                kill_grace_deadline = $3
            WHERE id = $1 AND status IN ('spawning', 'healthy', 'stuck')
            "#,
        )
        .bind(session_id)
        .bind(exit_reason)
        .bind(grace_deadline)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn mark_session_stuck(&self, session_id: Uuid) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE worker_sessions SET status = 'stuck'
            WHERE id = $1 AND status = 'healthy'
            "#,
        )
        .bind(session_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_session(&self, session_id: Uuid) -> Result<Option<WorkerSessionRow>> {
        sqlx::query_as::<_, WorkerSessionRow>(
            r#"
            SELECT id, project_id, run_id, board_task_id, attempt, status,
                   runtime_backend, runtime_handle, worker_token_hash, worker_profile,
                   last_heartbeat_at, progress_pct, exit_reason, kill_grace_deadline,
                   work_dir, started_at, exited_at, created_at
            FROM worker_sessions WHERE id = $1
            "#,
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(Into::into)
    }

    pub async fn list_active_run_ids_not_cancelling(&self, project_id: Uuid) -> Result<Vec<Uuid>> {
        let rows = sqlx::query_as::<_, (Uuid,)>(
            r#"
            SELECT id FROM work_runs
            WHERE project_id = $1 AND status = 'active' AND cancel_requested_at IS NULL
            "#,
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.0).collect())
    }

    pub async fn list_cancelling_runs(&self, project_id: Uuid) -> Result<Vec<super::WorkRunRow>> {
        sqlx::query_as::<_, super::WorkRunRow>(
            r#"
            SELECT id, project_id, status, source, cancel_requested_at,
                   orchestration_owned_at, first_spawn_at, terminal_at,
                   created_at, updated_at
            FROM work_runs
            WHERE project_id = $1
              AND cancel_requested_at IS NOT NULL
              AND status NOT IN ('completed', 'failed', 'cancelled')
            "#,
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
        .map_err(Into::into)
    }

    pub async fn cancel_nonterminal_tasks_without_session(
        &self,
        run_id: Uuid,
    ) -> Result<u64> {
        let res = sqlx::query(
            r#"
            UPDATE board_tasks
            SET status = 'cancelled',
                failure_reason = 'cancel',
                terminal_at = now(),
                updated_at = now()
            WHERE run_id = $1
              AND status NOT IN ('done_success', 'done_failed', 'cancelled')
              AND claimed_session_id IS NULL
            "#,
        )
        .bind(run_id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    pub async fn apply_hitl_resume(&self, project_id: Uuid) -> Result<u64> {
        // Step 2b: blocked + resume intent → ready; skip if run cancelling
        let res = sqlx::query(
            r#"
            UPDATE board_tasks t
            SET status = 'ready',
                hitl_resolved_at = now(),
                hitl_resume_requested_at = NULL,
                claimed_session_id = NULL,
                block_reason = NULL,
                updated_at = now()
            FROM work_runs r
            WHERE t.project_id = $1
              AND t.run_id = r.id
              AND t.status = 'blocked'
              AND t.hitl_resume_requested_at IS NOT NULL
              AND r.cancel_requested_at IS NULL
            "#,
        )
        .bind(project_id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }
}

#[derive(Debug, Clone)]
pub struct SpawnAssignment {
    pub session_id: Uuid,
    pub attempt: i32,
    pub worker_token: String,
}


impl Store {
    pub async fn get_project(&self, project_id: Uuid) -> Result<Option<super::ProjectRow>> {
        sqlx::query_as::<_, super::ProjectRow>(
            r#"SELECT id, name, created_at FROM projects WHERE id = $1"#,
        )
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(Into::into)
    }

    pub async fn get_run(&self, project_id: Uuid, run_id: Uuid) -> Result<Option<super::WorkRunRow>> {
        sqlx::query_as::<_, super::WorkRunRow>(
            r#"
            SELECT id, project_id, status, source, cancel_requested_at,
                   orchestration_owned_at, first_spawn_at, terminal_at,
                   created_at, updated_at
            FROM work_runs
            WHERE id = $1 AND project_id = $2
            "#,
        )
        .bind(run_id)
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(Into::into)
    }

    pub async fn list_runs(&self, project_id: Uuid) -> Result<Vec<super::WorkRunRow>> {
        sqlx::query_as::<_, super::WorkRunRow>(
            r#"
            SELECT id, project_id, status, source, cancel_requested_at,
                   orchestration_owned_at, first_spawn_at, terminal_at,
                   created_at, updated_at
            FROM work_runs
            WHERE project_id = $1
            ORDER BY created_at DESC
            "#,
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
        .map_err(Into::into)
    }

    pub async fn list_tasks(
        &self,
        project_id: Uuid,
        run_id: Option<Uuid>,
        status: Option<&str>,
    ) -> Result<Vec<super::BoardTaskRow>> {
        // Simple filter in SQL with optional binds via dynamic approach
        let tasks = sqlx::query_as::<_, super::BoardTaskRow>(
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
              AND ($2::uuid IS NULL OR run_id = $2)
              AND ($3::text IS NULL OR status = $3)
            ORDER BY priority DESC, created_at ASC
            "#,
        )
        .bind(project_id)
        .bind(run_id)
        .bind(status)
        .fetch_all(&self.pool)
        .await?;
        Ok(tasks)
    }

    pub async fn get_task(
        &self,
        project_id: Uuid,
        task_id: Uuid,
    ) -> Result<Option<super::BoardTaskRow>> {
        sqlx::query_as::<_, super::BoardTaskRow>(
            r#"
            SELECT id, project_id, run_id, plan_id, plan_task_id, title, summary, spec_md,
                   status, depends_on, priority, worker_profile, acceptance, external_ref,
                   timeout_s, max_retries, attempt, claimed_session_id, last_report_seq,
                   progress_pct, block_reason, hitl_notes, hitl_artifacts,
                   hitl_resume_requested_at, hitl_notified_at, hitl_resolved_at,
                   failure_reason, artifacts, retry_not_before,
                   assigned_at, started_at, terminal_at, created_at, updated_at
            FROM board_tasks
            WHERE id = $1 AND project_id = $2
            "#,
        )
        .bind(task_id)
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(Into::into)
    }

    /// HITL resume intent only (Model A). Returns false if task not blocked.
    pub async fn request_hitl_resume(
        &self,
        project_id: Uuid,
        task_id: Uuid,
        message: &str,
    ) -> Result<bool> {
        let res = sqlx::query(
            r#"
            UPDATE board_tasks
            SET hitl_notes = $3,
                hitl_resume_requested_at = now(),
                updated_at = now()
            WHERE id = $1 AND project_id = $2 AND status = 'blocked'
            "#,
        )
        .bind(task_id)
        .bind(project_id)
        .bind(message)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    pub async fn list_sessions_for_project(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<WorkerSessionRow>> {
        sqlx::query_as::<_, WorkerSessionRow>(
            r#"
            SELECT id, project_id, run_id, board_task_id, attempt, status,
                   runtime_backend, runtime_handle, worker_token_hash, worker_profile,
                   last_heartbeat_at, progress_pct, exit_reason, kill_grace_deadline,
                   work_dir, started_at, exited_at, created_at
            FROM worker_sessions
            WHERE project_id = $1
            ORDER BY created_at DESC
            "#,
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
        .map_err(Into::into)
    }

    pub async fn authenticate_worker_session(
        &self,
        session_id: Uuid,
        token: &str,
    ) -> Result<Option<WorkerSessionRow>> {
        let Some(s) = self.get_session(session_id).await? else {
            return Ok(None);
        };
        if crate::store::verify_worker_token(token, &s.worker_token_hash) {
            Ok(Some(s))
        } else {
            Ok(None)
        }
    }
}
