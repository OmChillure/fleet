//! Redis job bus (lab stand-in for Kafka/NATS/Zenoh).

use crate::job::{Job, JobResult};
use anyhow::{Context, Result};
use redis::AsyncCommands;
use tracing::info;

pub struct Bus {
    conn: redis::aio::MultiplexedConnection,
    jobs: String,
    results: String,
}

impl Bus {
    pub async fn connect(url: &str, jobs: &str, results: &str) -> Result<Self> {
        info!(%url, "connecting Redis");
        let client = redis::Client::open(url).context("redis client")?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .context("redis connect")?;
        // ping
        let mut c = conn.clone();
        let pong: String = redis::cmd("PING").query_async(&mut c).await?;
        info!(%pong, "Redis OK");
        Ok(Self {
            conn,
            jobs: jobs.into(),
            results: results.into(),
        })
    }

    pub async fn blpop_job(&mut self, timeout_sec: f64) -> Result<Option<Job>> {
        let res: Option<(String, String)> = self
            .conn
            .blpop(&self.jobs, timeout_sec)
            .await
            .context("BLPOP")?;
        match res {
            None => Ok(None),
            Some((_key, raw)) => {
                info!(bytes = raw.len(), "received job");
                let job: Job = serde_json::from_str(&raw).context("parse job JSON")?;
                Ok(Some(job))
            }
        }
    }

    pub async fn publish_result(&mut self, result: &JobResult) -> Result<()> {
        let payload = serde_json::to_string(result)?;
        let _: () = self.conn.lpush(&self.results, &payload).await?;
        let key = format!("fleet:result:{}", result.job_id);
        let _: () = self.conn.set_ex(&key, &payload, 86400).await?;
        info!(
            job_id = %result.job_id,
            harness = ?result.harness,
            ok = result.ok,
            "published result"
        );
        Ok(())
    }

    pub async fn seed_if_empty(&mut self, job: &Job) -> Result<()> {
        let len: i64 = self.conn.llen(&self.jobs).await?;
        if len > 0 {
            info!(len, "queue non-empty; skip seed");
            return Ok(());
        }
        let payload = serde_json::to_string(job)?;
        let _: () = self.conn.lpush(&self.jobs, payload).await?;
        info!(job_id = %job.job_id, "seeded job");
        Ok(())
    }
}
