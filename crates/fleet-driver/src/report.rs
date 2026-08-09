//! Persist turn artifacts under the session work dir.

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

pub fn write_artifacts(work_dir: &Path, result: &Value, transcript: &str) -> Result<()> {
    let dir = work_dir.join("artifacts");
    std::fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
    std::fs::write(dir.join("transcript.md"), transcript)
        .with_context(|| "write artifacts/transcript.md")?;
    let body = serde_json::to_vec_pretty(result).context("serialize result.json")?;
    std::fs::write(dir.join("result.json"), body).context("write artifacts/result.json")?;
    Ok(())
}
