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
    // Process/docker validate_done requires host `result.md` and, for coding
    // profiles, a `work/` directory.
    std::fs::create_dir_all(work_dir.join("work"))
        .with_context(|| format!("mkdir {}", work_dir.join("work").display()))?;
    let summary = result
        .get("stop_reason")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("completed");
    let mut md = format!("# Result\n\nACP {summary}.\n\n");
    let t = transcript.trim();
    if !t.is_empty() {
        md.push_str("## Transcript\n\n");
        md.push_str(t);
        md.push('\n');
    } else {
        md.push_str("No transcript was produced.\n");
    }
    std::fs::write(work_dir.join("result.md"), md).context("write result.md")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn write_artifacts_makes_result_md_long_enough_for_validate_done() {
        let dir = tempfile::tempdir().unwrap();
        write_artifacts(
            dir.path(),
            &json!({ "ok": true, "stop_reason": "end_turn" }),
            "hello from mock agent",
        )
        .unwrap();
        let md = std::fs::read_to_string(dir.path().join("result.md")).unwrap();
        assert!(md.trim().chars().count() >= 20, "{md}");
        assert!(dir.path().join("work").is_dir());
        assert!(md.contains("hello from mock agent"), "{md}");
    }
}
