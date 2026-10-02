//! Per-repo default place and network.

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::models::{parse_egress_mode, parse_worker_host_run_mode};
use crate::pool::map_query_err;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoPosture {
    pub repo: String,
    pub run_mode: String,
    pub egress_mode: String,
    pub egress_allowlist: String,
}

pub fn normalize_repo(raw: &str) -> Result<String, DbError> {
    let repo = raw.trim().trim_matches('/');
    if repo.is_empty() || repo.len() > 200 || repo.contains(' ') {
        return Err(DbError::Conflict(
            "repo must be a name like owner/name".into(),
        ));
    }
    Ok(repo.to_string())
}

pub async fn list_for_workspace(pool: &PgPool, workspace_id: Uuid) -> DbResult<Vec<RepoPosture>> {
    let rows = sqlx::query_as::<_, (String, String, String, String)>(
        r#"
        SELECT repo, run_mode, egress_mode, egress_allowlist
        FROM repo_postures
        WHERE workspace_id = $1
        ORDER BY repo
        "#,
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await
    .map_err(map_query_err)?;
    Ok(rows
        .into_iter()
        .map(
            |(repo, run_mode, egress_mode, egress_allowlist)| RepoPosture {
                repo,
                run_mode,
                egress_mode,
                egress_allowlist,
            },
        )
        .collect())
}

pub async fn get(pool: &PgPool, workspace_id: Uuid, repo: &str) -> DbResult<Option<RepoPosture>> {
    let repo = normalize_repo(repo)?;
    let row = sqlx::query_as::<_, (String, String, String, String)>(
        r#"
        SELECT repo, run_mode, egress_mode, egress_allowlist
        FROM repo_postures
        WHERE workspace_id = $1 AND repo = $2
        "#,
    )
    .bind(workspace_id)
    .bind(&repo)
    .fetch_optional(pool)
    .await
    .map_err(map_query_err)?;
    Ok(row.map(
        |(repo, run_mode, egress_mode, egress_allowlist)| RepoPosture {
            repo,
            run_mode,
            egress_mode,
            egress_allowlist,
        },
    ))
}

pub async fn upsert(
    pool: &PgPool,
    workspace_id: Uuid,
    repo: &str,
    run_mode: &str,
    egress_mode: &str,
    egress_allowlist: &str,
) -> DbResult<RepoPosture> {
    let repo = normalize_repo(repo)?;
    let run_mode = parse_worker_host_run_mode(run_mode)
        .ok_or_else(|| DbError::Conflict("repo run_mode must be on_pc or in_container".into()))?;
    let egress_mode = parse_egress_mode(egress_mode).ok_or_else(|| {
        DbError::Conflict("repo egress_mode must be inherit, none, allowlist, or open".into())
    })?;
    let list = egress_allowlist.trim();
    if list.len() > 2000 {
        return Err(DbError::Conflict(
            "egress_allowlist must be at most 2000 characters".into(),
        ));
    }
    let row = sqlx::query_as::<_, (String, String, String, String)>(
        r#"
        INSERT INTO repo_postures (
            workspace_id, repo, run_mode, egress_mode, egress_allowlist
        )
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (workspace_id, repo) DO UPDATE
        SET run_mode = EXCLUDED.run_mode,
            egress_mode = EXCLUDED.egress_mode,
            egress_allowlist = EXCLUDED.egress_allowlist,
            updated_at = now()
        RETURNING repo, run_mode, egress_mode, egress_allowlist
        "#,
    )
    .bind(workspace_id)
    .bind(&repo)
    .bind(run_mode)
    .bind(egress_mode)
    .bind(list)
    .fetch_one(pool)
    .await
    .map_err(map_query_err)?;
    Ok(RepoPosture {
        repo: row.0,
        run_mode: row.1,
        egress_mode: row.2,
        egress_allowlist: row.3,
    })
}
