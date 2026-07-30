#!/usr/bin/env python3
"""Publish job JSON into the task sandbox redis.

Job file fields win over .env for harness (so HARNESS=opencode in .env
does not clobber harness=pi jobs). Override with FLEET_FORCE_HARNESS=1.
"""
import json
import os
import subprocess
import sys
from pathlib import Path

job = json.loads(Path(os.environ["JOB_FILE"]).read_text())
job["job_id"] = os.environ["JOB_ID"]

if os.environ.get("OPENCODE_BASE_URL") and job.get("provider"):
    job["provider"]["base_url"] = os.environ["OPENCODE_BASE_URL"]
if os.environ.get("MODEL"):
    job["model"] = os.environ["MODEL"]

# Job JSON harness wins unless empty or FLEET_FORCE_HARNESS=1
env_h = (os.environ.get("HARNESS") or "").strip()
force = os.environ.get("FLEET_FORCE_HARNESS", "").strip() in ("1", "true", "yes")
if env_h and (force or not job.get("harness")):
    job["harness"] = env_h

harness = (job.get("harness") or "opencode").lower()
# Sanity: pi image + opencode harness is a common misconfig
if "pi" in os.environ.get("DRIVER_BASE_IMAGE", "") and harness not in (
    "pi",
    "pi-dev",
    "pi_acp",
):
    print(
        f"warning: DRIVER_BASE_IMAGE is pi-like but harness={harness!r}",
        file=sys.stderr,
    )

subprocess.run(
    [
        "docker",
        "compose",
        "-p",
        os.environ["COMPOSE_PROJECT_NAME"],
        "exec",
        "-T",
        "redis",
        "redis-cli",
        "-x",
        "LPUSH",
        os.environ.get("FLEET_JOBS_QUEUE", "fleet:jobs"),
    ],
    input=json.dumps(job) + "\n",
    text=True,
    check=True,
    stdout=subprocess.DEVNULL,
)
print(
    f"published harness={job.get('harness')} model={job.get('model')}",
    flush=True,
)
