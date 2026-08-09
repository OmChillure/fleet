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


def apply_env_overlay(job: dict) -> dict:
    """Overlay .env provider/model onto job JSON (LM Studio fixtures → local Ollama)."""
    prov = job.get("provider")
    if isinstance(prov, dict):
        if os.environ.get("OPENCODE_BASE_URL"):
            prov["base_url"] = os.environ["OPENCODE_BASE_URL"]
        if os.environ.get("OPENCODE_PROVIDER_ID"):
            prov["id"] = os.environ["OPENCODE_PROVIDER_ID"]
        if os.environ.get("OPENCODE_PROVIDER_NAME"):
            prov["name"] = os.environ["OPENCODE_PROVIDER_NAME"]
        if os.environ.get("OPENCODE_API_KEY"):
            prov["api_key"] = os.environ["OPENCODE_API_KEY"]
        if os.environ.get("OPENCODE_PROVIDER_NPM"):
            prov["npm"] = os.environ["OPENCODE_PROVIDER_NPM"]
    if os.environ.get("MODEL"):
        job["model"] = os.environ["MODEL"]
        if isinstance(prov, dict):
            mid = os.environ["MODEL"]
            pid = (prov.get("id") or "").strip()
            if pid and mid.startswith(pid + "/"):
                mid = mid[len(pid) + 1 :]
            models = prov.setdefault("models", {})
            if isinstance(models, dict) and mid not in models:
                models[mid] = {"name": mid}
    env_h = (os.environ.get("HARNESS") or "").strip()
    force = os.environ.get("FLEET_FORCE_HARNESS", "").strip() in ("1", "true", "yes")
    if env_h and (force or not job.get("harness")):
        job["harness"] = env_h
    return job


def image_is_pi(img: str) -> bool:
    """True only for a Pi harness image — not 'alpine' (contains the letters pi)."""
    name = img.rsplit("/", 1)[-1].split(":")[0].lower()
    return name in {"pi", "agent-fleet-pi"} or name.endswith("-pi")


if __name__ == "__main__":
    job = json.loads(Path(os.environ["JOB_FILE"]).read_text())
    job["job_id"] = os.environ["JOB_ID"]
    apply_env_overlay(job)

    harness = (job.get("harness") or "opencode").lower()
    # Sanity: pi image + non-pi harness is a common misconfig
    if image_is_pi(os.environ.get("DRIVER_BASE_IMAGE", "")) and harness not in (
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
