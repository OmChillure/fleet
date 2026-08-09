use thiserror::Error;

/// Errors from the database layer (pool, migrations, queries).
#[derive(Debug, Error)]
pub enum DbError {
    #[error(
        "could not connect to Postgres at `{target}`: {source}\n\
         Hint: set DATABASE_URL in `.env` to a reachable DB.\n\
         - Neon/Supabase: paste the pooled connection string and include `?sslmode=require`\n\
         - Local: start Postgres and create the database, or point DATABASE_URL at cloud"
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
    pub fn is_unique_violation(&self) -> bool {
        match self {
            DbError::Sqlx(sqlx::Error::Database(db)) => db.code().as_deref() == Some("23505"),
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
