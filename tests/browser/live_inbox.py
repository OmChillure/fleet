#!/usr/bin/env python3
"""Seed live inbox rows on the VPS Postgres (no OAuth)."""
from __future__ import annotations

import subprocess
from pathlib import Path


def _lit(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def seed_live_ticket(
    workspace_id: str,
    provider: str,
    key: str,
    title: str,
    summary: str,
) -> None:
    sql = (
        "INSERT INTO ticket_inbox ("
        "workspace_id, provider, external_key, title, url, state, summary, last_seen_at"
        ") VALUES ("
        f"{_lit(workspace_id)}, {_lit(provider)}, {_lit(key)}, {_lit(title)}, "
        f"NULL, 'open', {_lit(summary)}, now()"
        ") ON CONFLICT (workspace_id, provider, external_key) DO UPDATE SET "
        "title = EXCLUDED.title, summary = EXCLUDED.summary, "
        "state = 'open', run_id = NULL, last_seen_at = now();"
    )
    remote = (
        "docker exec -i agent-fleet-postgres-1 "
        "psql -U fleet -d fleet -v ON_ERROR_STOP=1 "
        f"-c {subprocess.list2cmdline([sql]).replace(chr(10), ' ')}"
    )
    # list2cmdline is messy for sql. Pass via stdin.
    subprocess.run(
        [
            "ssh",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=8",
            "-i",
            str(Path.home() / ".ssh/agent-fleet"),
            "agent-fleet@82.39.213.33",
            "docker exec -i agent-fleet-postgres-1 "
            "psql -U fleet -d fleet -v ON_ERROR_STOP=1",
        ],
        input=sql + "\n",
        check=True,
        text=True,
        capture_output=True,
    )
