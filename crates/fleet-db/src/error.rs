use thiserror::Error;

/// Errors from the database layer (pool, migrations, queries).
#[derive(Debug, Error)]
pub enum DbError {
    #[error(
        "could not connect to Postgres at `{target}`: {source}\n\
         Hint: set DATABASE_URL in `.env` to a reachable Postgres.\n\
         - Local: start Postgres or `just db-up`\n\
         - Supabase/Neon: session pooler or direct URI + `?sslmode=require`\n\
         - Prefer Supabase :5432 (session). :6543 is rewritten automatically.\n\
         - Slow connect: DATABASE_ACQUIRE_TIMEOUT_SECS=90"
    )]
    Connect {
        target: String,
        #[source]
        source: sqlx::Error,
    },

    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),

    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),

    #[error("{0}")]
    Conflict(String),

    #[error("{0}")]
    NotFound(String),
}

impl DbError {
    /// True when this error is (or was mapped from) a Postgres unique violation (23505).
    ///
    /// [`Self::from_sqlx`] turns 23505 into [`DbError::Conflict`] before callers
    /// see the raw sqlx code, so matching only `DbError::Sqlx` would always lie
    /// after `map_query_err`. All `Conflict` variants in this crate currently
    /// mean uniqueness (generic message or a domain-specific one such as email).
    pub fn is_unique_violation(&self) -> bool {
        match self {
            DbError::Sqlx(sqlx::Error::Database(db)) => db.code().as_deref() == Some("23505"),
            DbError::Conflict(_) => true,
            _ => false,
        }
    }

    pub fn from_sqlx(err: sqlx::Error) -> Self {
        if let sqlx::Error::Database(ref db) = err
            && db.code().as_deref() == Some("23505") {
                tracing::debug!(
                    detail = %db.message(),
                    constraint = ?db.constraint(),
                    "unique constraint violation"
                );
                return DbError::Conflict("unique constraint violated".into());
            }
        DbError::Sqlx(err)
    }
}

pub type DbResult<T> = Result<T, DbError>;
