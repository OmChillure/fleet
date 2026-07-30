#!/usr/bin/env python3
"""Compact lab result printer.

Prefers env FLEET_LAB_JOB_ID / FLEET_LAB_COMPOSE_PROJECT (per-task).
Falls back to /tmp/fleet-lab-* for older callers.
Can also read a cached result JSON via FLEET_LAB_RESULT_FILE.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path


def main() -> int:
    jid = (
        os.environ.get("FLEET_LAB_JOB_ID")
        or Path("/tmp/fleet-lab-job-id").read_text().strip()
        if Path("/tmp/fleet-lab-job-id").is_file()
        else ""
    )
    proj = (
        os.environ.get("FLEET_LAB_COMPOSE_PROJECT")
        or (
            Path("/tmp/fleet-lab-compose-project").read_text().strip()
            if Path("/tmp/fleet-lab-compose-project").is_file()
            else ""
        )
    )
    if not jid or not proj:
        print("  error: missing job_id / compose project", file=sys.stderr)
        return 1

    full = os.environ.get("FULL_TRANSCRIPT") == "1"
    limit = int(os.environ.get("TRANSCRIPT_CHARS") or "1200")
    verbose = os.environ.get("VERBOSE") == "1"
    cached = os.environ.get("FLEET_LAB_RESULT_FILE", "").strip()

    def redis_get(k: str) -> str:
        try:
            out = subprocess.check_output(
                [
                    "docker",
                    "compose",
                    "-p",
                    proj,
                    "exec",
                    "-T",
                    "redis",
                    "redis-cli",
                    "GET",
                    k,
                ],
                text=True,
                stderr=subprocess.STDOUT,
            ).strip()
        except subprocess.CalledProcessError as e:
            print(f"  error: redis get failed ({e})", file=sys.stderr)
            return ""
        return "" if out in ("", "(nil)") else out

    raw = ""
    if cached and Path(cached).is_file():
        raw = Path(cached).read_text().strip()
    if not raw:
        raw = redis_get(f"fleet:result:{jid}")
    if not raw:
        print(f"  error: no result for {jid} project={proj}", file=sys.stderr)
        return 1

    d = json.loads(raw)
    acp = d.get("acp") or d.get("opencode") or {}
    ok = bool(d.get("ok"))
    tools = ((d.get("provision") or {}).get("detail") or {}).get("tools") or []
    milestones = (d.get("provision") or {}).get("milestones") or []

    print(
        f"  ok={ok}  harness={acp.get('harness') or d.get('harness')}  "
        f"model={acp.get('model')}"
    )
    print(f"  elapsed={d.get('elapsed_sec')}s  stop={acp.get('stop_reason')}")
    if d.get("error"):
        print(f"  error={d.get('error')}")
    if tools:
        t = ", ".join(tools) if isinstance(tools, list) else tools
        print(f"  tools={t}")
    if verbose and milestones:
        print(f"  milestones={milestones}")

    def ws_test(path: str) -> bool:
        r = subprocess.run(
            [
                "docker",
                "compose",
                "-p",
                proj,
                "exec",
                "-T",
                "workspace",
                "test",
                "-f",
                path,
            ],
            capture_output=True,
        )
        return r.returncode == 0

    repo = f"/workspace/jobs/{jid}/repo"
    found = []
    for rel in [
        "app/fleet_marker.py",
        "tests/test_fleet_smoke.py",
        "invoices/models/animal.py",
        "tests/test_fleet_animal_notes.py",
        ".fleet-tools.json",
    ]:
        if ws_test(f"{repo}/{rel}"):
            found.append(rel)
    print(f"  files: {', '.join(found) if found else '(none of expected)'}")

    analysis = (d.get("analysis") or acp.get("stdout_tail") or "").strip()
    if analysis:
        print("  --- transcript ---")
        if full or len(analysis) <= limit:
            print(analysis)
        else:
            print(analysis[:limit])
            print(
                f"  … truncated ({len(analysis)} chars; FULL_TRANSCRIPT=1 for all)"
            )
    else:
        print("  transcript: (empty)")

    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
