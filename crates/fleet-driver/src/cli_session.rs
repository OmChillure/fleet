//! One-shot official CLI session (Claude print mode with tools).
//! Grok stays on ACP. Chat's Claude path disables tools; fleet needs them.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Result, anyhow};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::harness::HarnessSpec;

#[derive(Debug)]
pub struct CliOutcome {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

pub async fn run_cli(
    spec: &HarnessSpec,
    cwd: &Path,
    prompt: &str,
    job_timeout: Duration,
) -> Result<CliOutcome> {
    let mut child = Command::new(&spec.command);
    child
        .args(&spec.args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = child
        .spawn()
        .map_err(|e| anyhow!("{}: {e}", spec.command))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.as_bytes()).await;
    }
    let out = match timeout(job_timeout, child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(anyhow!("{}: {e}", spec.command)),
        Err(_) => {
            return Err(anyhow!(
                "{} timed out after {}s",
                spec.command,
                job_timeout.as_secs()
            ));
        }
    };
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success() {
        let err = stderr.trim();
        if err.is_empty() {
            return Err(anyhow!("{} exited {}", spec.command, out.status));
        }
        return Err(anyhow!("{} failed: {err}", spec.command));
    }
    Ok(CliOutcome {
        ok: true,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_bin_errs() {
        let spec = HarnessSpec {
            name: "claude".into(),
            command: "fleet-no-such-cli-bin".into(),
            args: vec!["-p".into()],
        };
        let err = run_cli(&spec, Path::new("."), "hi", Duration::from_secs(2))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("fleet-no-such-cli-bin"), "{err}");
    }
}
