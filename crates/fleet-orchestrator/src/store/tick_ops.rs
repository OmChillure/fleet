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
        work_dir: Option<&str>,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
            UPDATE worker_sessions
            SET runtime_handle = $2,
                work_dir = COALESCE($3, work_dir),
                status = 'healthy',
                last_heartbeat_at = now(),
                started_at = COALESCE(started_at, now())
            WHERE id = $1
            "#,
        )
        .bind(session_id)
        .bind(runtime_handle)
        .bind(work_dir)
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

    /// Remote enqueue: keep `spawning` until the job worker heartbeats.
    /// Does not stamp `last_heartbeat_at` (that would start the stale clock).
    pub async fn record_remote_enqueued(
        &self,
        session_id: Uuid,
        runtime_handle: &str,
        work_dir: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE worker_sessions
            SET runtime_backend = 'remote',
                runtime_handle = $2,
                work_dir = COALESCE($3, work_dir)
            WHERE id = $1 AND status = 'spawning'
            "#,
        )
        .bind(session_id)
        .bind(runtime_handle)
        .bind(work_dir)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Mark ready/leased `spawn_session` claims dead. Leaves `signal_session`
    /// so a kill can still be leased.
    pub async fn void_spawn_claims(&self, session_id: Uuid) -> Result<()> {
        self.void_session_claims(session_id, &["spawn_session"], "session abandoned")
            .await
    }

    async fn void_session_claims(
        &self,
        session_id: Uuid,
        kinds: &[&str],
        last_error: &str,
    ) -> Result<()> {
        if let Err(e) = sqlx::query(
            r#"
            UPDATE work_claims
            SET status = 'dead',
                last_error = $2,
                lease_owner = NULL,
                lease_token = NULL,
                lease_expires_at = NULL,
                version = version + 1,
                updated_at = now()
            WHERE kind = ANY($3)
              AND payload->>'session_id' = $1
              AND status IN ('ready', 'leased')
            "#,
        )
        .bind(session_id.to_string())
        .bind(last_error)
        .bind(
            kinds
                .iter()
                .map(|s| (*s).to_string())
                .collect::<Vec<String>>(),
        )
        .execute(&self.pool)
        .await
        {
            // Orch-only test DBs have no fleet-db tables. Production always does.
            let missing = matches!(
                &e,
                sqlx::Error::Database(db) if db.code().as_deref() == Some("42P01")
            );
            if !missing {
                return Err(e.into());
            }
        }
        Ok(())
    }

    /// Only a still-spawning row may become `failed_spawn`. Replace/kill already
    /// mark `exited`; a late host "spawn failed" must not overwrite that or the
    /// retry path will reopen the task under a living sibling.
    pub async fn complete_spawn_failure(&self, session_id: Uuid) -> Result<bool> {
        self.void_session_claims(
            session_id,
            &["spawn_session", "signal_session"],
            "session spawn failed",
        )
        .await?;
        let res = sqlx::query(
            r#"
            UPDATE worker_sessions
            SET status = 'failed_spawn',
                exit_reason = 'spawn_failed',
                exited_at = now()
            WHERE id = $1 AND status = 'spawning'
            "#,
        )
        .bind(session_id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Exit every live session on a task except `keep` (the replacement).
    /// Returns the rows that were still live so the caller can signal kill.
    pub async fn retire_live_sessions(
        &self,
        task_id: Uuid,
        keep: Option<Uuid>,
        reason: &str,
    ) -> Result<Vec<WorkerSessionRow>> {
        let rows = sqlx::query_as::<_, WorkerSessionRow>(
            r#"
            UPDATE worker_sessions
            SET status = 'exited',
                exit_reason = $2,
                exited_at = COALESCE(exited_at, now())
            WHERE board_task_id = $1
              AND status IN ('spawning', 'healthy', 'stuck', 'killing')
              AND ($3::uuid IS NULL OR id <> $3)
            RETURNING id, project_id, run_id, board_task_id, attempt, status,
                      runtime_backend, runtime_handle, worker_token_hash,
                      worker_profile, last_heartbeat_at, progress_pct, exit_reason,
                      kill_grace_deadline, work_dir, started_at, exited_at, created_at
            "#,
        )
        .bind(task_id)
        .bind(reason)
        .bind(keep)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn task_has_live_session(&self, task_id: Uuid) -> Result<bool> {
        let row = sqlx::query_as::<_, (bool,)>(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM worker_sessions
                WHERE board_task_id = $1
                  AND status IN ('spawning', 'healthy', 'stuck', 'killing')
            )
            "#,
        )
        .bind(task_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.0)
    }

    /// True when the task is claimed by a still-live session from a later attempt.
    /// A stale spawn-failed retry must not bounce that claim back to `ready`.
    pub async fn live_claim_is_other_attempt(&self, task_id: Uuid, attempt: i32) -> Result<bool> {
        let row = sqlx::query_as::<_, (i32, String)>(
            r#"
            SELECT s.attempt, s.status
            FROM board_tasks t
            JOIN worker_sessions s ON s.id = t.claimed_session_id
            WHERE t.id = $1
            "#,
        )
        .bind(task_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some_and(|(a, st)| {
            a != attempt && matches!(st.as_str(), "spawning" | "healthy" | "stuck" | "killing")
        }))
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
                progress_pct = COALESCE($2, progress_pct),
                status = CASE WHEN status = 'spawning' THEN 'healthy' ELSE status END,
                started_at = COALESCE(started_at, now())
            WHERE id = $1
              AND status IN ('spawning', 'healthy', 'stuck')
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
                status = CASE WHEN status = 'assigned' THEN 'in_progress' ELSE status END,
                started_at = COALESCE(started_at, now()),
                updated_at = now()
            WHERE id = $1
              AND claimed_session_id = $4
            "#,
        )
        .bind(task_id)
        .bind(seq)
        .bind(progress_pct)
        .bind(session_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Worker terminal (`done_success` / `done_failed` / `blocked`) CAS from
    /// assigned/in_progress with matching claimed_session_id. Blocked HITL is
    /// not a from-status. Success/fail clear the claim (caller destroys runtime).
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
                WHERE id = $1
                  AND status IN ('assigned', 'in_progress')
                  AND claimed_session_id = $4
                "#,
            )
            .bind(task_id)
            .bind(failure_or_block)
            .bind(seq)
            .bind(session_id)
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
                WHERE id = $1
                  AND status IN ('assigned', 'in_progress')
                  AND claimed_session_id = $5
                "#,
            )
            .bind(task_id)
            .bind(status)
            .bind(failure_or_block)
            .bind(seq)
            .bind(session_id)
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

    /// Bounce a finished (or live) task back to `ready` with reviewer notes.
    pub async fn reopen_task_for_changes(
        &self,
        task_id: Uuid,
        reason: &str,
        notes_append: &str,
    ) -> Result<()> {
        let notes = notes_append.chars().take(4000).collect::<String>();
        sqlx::query(
            r#"
            UPDATE board_tasks
            SET status = 'ready',
                claimed_session_id = NULL,
                terminal_at = NULL,
                failure_reason = $2,
                hitl_notes = CASE
                    WHEN hitl_notes IS NULL OR hitl_notes = '' THEN $3
                    ELSE hitl_notes || E'\n\n' || $3
                END,
                spec_md = CASE
                    WHEN length(spec_md) > 56000 THEN spec_md
                    ELSE spec_md || E'\n\n' || $3
                END,
                updated_at = now()
            WHERE id = $1
              AND status NOT IN ('cancelled', 'done_failed')
            "#,
        )
        .bind(task_id)
        .bind(reason)
        .bind(notes)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Put a task back on the dependency queue (reviewer waits on bounced deps).
    pub async fn requeue_task_waiting(&self, task_id: Uuid, reason: &str) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE board_tasks
            SET status = 'queued',
                claimed_session_id = NULL,
                terminal_at = NULL,
                failure_reason = $2,
                updated_at = now()
            WHERE id = $1
              AND status NOT IN ('cancelled', 'done_failed')
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

    pub async fn cancel_nonterminal_tasks_without_session(&self, run_id: Uuid) -> Result<u64> {
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

    pub async fn get_run(
        &self,
        project_id: Uuid,
        run_id: Uuid,
    ) -> Result<Option<super::WorkRunRow>> {
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

    /// Projects with at least one non-terminal run (for the local background ticker).
    pub async fn list_open_project_ids(&self) -> Result<Vec<Uuid>> {
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            r#"
            SELECT DISTINCT project_id
            FROM work_runs
            WHERE status NOT IN ('completed', 'failed', 'cancelled')
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
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

    /// UI/ticker list — skips bulky spec_md so Neon polls stay cheap.
    pub async fn list_tasks_slim(
        &self,
        project_id: Uuid,
        run_id: Option<Uuid>,
    ) -> Result<Vec<super::BoardTaskRow>> {
        let tasks = sqlx::query_as::<_, super::BoardTaskRow>(
            r#"
            SELECT id, project_id, run_id, plan_id, plan_task_id, title, summary,
                   ''::text AS spec_md,
                   status, depends_on, priority, worker_profile, acceptance, external_ref,
                   timeout_s, max_retries, attempt, claimed_session_id, last_report_seq,
                   progress_pct, block_reason, hitl_notes,
                   NULL::jsonb AS hitl_artifacts,
                   hitl_resume_requested_at, hitl_notified_at, hitl_resolved_at,
                   failure_reason, artifacts, retry_not_before,
                   assigned_at, started_at, terminal_at, created_at, updated_at
            FROM board_tasks
            WHERE project_id = $1
              AND ($2::uuid IS NULL OR run_id = $2)
            ORDER BY priority DESC, created_at ASC
            "#,
        )
        .bind(project_id)
        .bind(run_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(tasks)
    }

    pub async fn list_tasks(
        &self,
        project_id: Uuid,
        run_id: Option<Uuid>,
        status: Option<&str>,
    ) -> Result<Vec<super::BoardTaskRow>> {
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

    pub async fn mark_hitl_notified(&self, task_id: Uuid) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE board_tasks
            SET hitl_notified_at = now(), updated_at = now()
            WHERE id = $1 AND hitl_notified_at IS NULL
            "#,
        )
        .bind(task_id)
        .execute(&self.pool)
        .await?;
        Ok(())
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

    /// In-process test sessions that still hold a plane slot.
    pub async fn count_local_capacity(&self, project_id: Uuid) -> Result<i32> {
        let rows = self.list_sessions_for_project(project_id).await?;
        Ok(rows
            .iter()
            .filter(|s| {
                s.runtime_backend != crate::sandbox::remote::BACKEND
                    && crate::domain::WorkerSessionStatus::from_db(&s.status)
                        .is_some_and(|st| st.holds_capacity())
            })
            .count() as i32)
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
        if matches!(s.status.as_str(), "exited" | "failed_spawn") {
            return Ok(None);
        }
        if crate::store::verify_worker_token(token, &s.worker_token_hash) {
            Ok(Some(s))
        } else {
            Ok(None)
        }
    }
}

impl Store {
    /// Create project with a fixed id if missing (workspace_id == project_id mapping).
    pub async fn ensure_project(
        &self,
        project_id: Uuid,
        name: &str,
        max_agents: i32,
    ) -> Result<super::ProjectRow> {
        if let Some(p) = self.get_project(project_id).await? {
            return Ok(p);
        }
        let mut tx = self.pool.begin().await?;
        let project = sqlx::query_as::<_, super::ProjectRow>(
            r#"
            INSERT INTO projects (id, name)
            VALUES ($1, $2)
            ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name
            RETURNING id, name, created_at
            "#,
        )
        .bind(project_id)
        .bind(name)
        .fetch_one(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO project_configs (project_id, max_agents)
            VALUES ($1, $2)
            ON CONFLICT (project_id) DO NOTHING
            "#,
        )
        .bind(project_id)
        .bind(max_agents)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(project)
    }

    /// Raise project max_agents. Never lowers an existing cap (GREATEST).
    pub async fn set_max_agents(&self, project_id: Uuid, max_agents: i32) -> Result<()> {
        let max = max_agents.clamp(1, crate::domain::MAX_PROJECT_AGENTS);
        sqlx::query(
            r#"
            UPDATE project_configs
            SET max_agents = GREATEST(max_agents, $2)
            WHERE project_id = $1
            "#,
        )
        .bind(project_id)
        .bind(max)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Workspace Settings: concurrent agents + local fleet LLM (Ollama). Not Grok.
    pub async fn update_fleet_settings(
        &self,
        project_id: Uuid,
        max_agents: i32,
        fleet_llm: &str,
        fleet_llm_model: &str,
    ) -> Result<super::ProjectConfigRow> {
        let max = max_agents.clamp(1, crate::domain::MAX_PROJECT_AGENTS);
        let llm = match fleet_llm.trim().to_ascii_lowercase().as_str() {
            "ollama" => "ollama",
            _ => "off",
        };
        let model = if llm == "off" {
            String::new()
        } else {
            fleet_llm_model.trim().chars().take(128).collect()
        };
        sqlx::query(
            r#"
            UPDATE project_configs
            SET max_agents = $2,
                fleet_llm = $3,
                fleet_llm_model = $4,
                updated_at = now()
            WHERE project_id = $1
            "#,
        )
        .bind(project_id)
        .bind(max)
        .bind(llm)
        .bind(&model)
        .execute(&self.pool)
        .await?;
        self.get_project_config(project_id).await
    }
}
