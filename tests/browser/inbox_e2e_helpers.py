#!/usr/bin/env python3
"""SQL seed helpers for Inbox browser e2e / record_demo.

Inbox rows come from poll (needs a connected integration) or chat
`finish_ticket` (not a page control). Playwright still has to click the
real Start monitor / Finish chrome, so tests insert a connected github
row plus an open ticket into the same Postgres the live API uses.
"""
from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
_DEFAULT_DB = "postgres://fleet:fleet@127.0.0.1:5433/fleet?sslmode=disable"
_KIND_CONTEXT = "kind-agent-fleet"
_KIND_NS = "agent-fleet"


def _load_env() -> dict[str, str]:
    out: dict[str, str] = {}
    for name in (".env", ".env.local"):
        p = REPO / name
        if not p.is_file():
            continue
        for line in p.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            k, v = line.split("=", 1)
            v = v.strip().strip("'").strip('"')
            out[k.strip()] = v
    return out


def database_url() -> str:
    env = _load_env()
    url = (
        os.environ.get("DATABASE_URL")
        or env.get("DATABASE_URL")
        or _DEFAULT_DB
    ).strip()
    if not url:
        raise RuntimeError(
            "DATABASE_URL is empty. Set it in repo-root .env "
            "(same file as JWT / OAuth) so inbox e2e can seed an open ticket."
        )
    return url


def _lit(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def _rows(out: str) -> list[list[str]]:
    rows: list[list[str]] = []
    for line in out.splitlines():
        if line:
            rows.append(line.split("\t"))
    return rows


def _run_sql(cmd: list[str]) -> list[list[str]]:
    try:
        out = subprocess.check_output(cmd, text=True, stderr=subprocess.STDOUT)
    except FileNotFoundError as e:
        raise RuntimeError(
            f"{cmd[0]} is not installed. Run "
            "crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh"
        ) from e
    except subprocess.CalledProcessError as e:
        raise RuntimeError(f"psql failed: {e.output[-800:]}") from e
    return _rows(out)


def sql(statement: str) -> list[list[str]]:
    return _run_sql(
        ["psql", database_url(), "-v", "ON_ERROR_STOP=1", "-At", "-F", "\t", "-c", statement]
    )


def _sql_kind(statement: str) -> list[list[str]]:
    """Kind extraPortMappings own :8080 — that API uses in-cluster postgres."""
    if shutil.which("kubectl") is None:
        raise RuntimeError("kubectl is not installed (needed to seed Kind postgres)")
    return _run_sql(
        [
            "kubectl",
            "--context",
            _KIND_CONTEXT,
            "-n",
            _KIND_NS,
            "exec",
            "deploy/postgres",
            "--",
            "psql",
            "-U",
            "fleet",
            "-d",
            "fleet",
            "-v",
            "ON_ERROR_STOP=1",
            "-At",
            "-F",
            "\t",
            "-c",
            statement,
        ]
    )


def _workspace_in(run, workspace_id: str) -> bool:
    try:
        return bool(run(f"SELECT 1 FROM workspaces WHERE id = {_lit(workspace_id)}"))
    except RuntimeError:
        return False


def sql_for_workspace(workspace_id: str, statement: str) -> list[list[str]]:
    """Seed the Postgres the live :8080 API actually uses (host or Kind)."""
    runners = [sql, _sql_kind]
    for run in runners:
        if _workspace_in(run, workspace_id):
            return run(statement)
    raise RuntimeError(
        f"workspace {workspace_id} is not in host DATABASE_URL or Kind "
        f"{_KIND_NS} postgres. Inbox e2e seeds the same database the API "
        "on :8080 uses (Kind extraPortMappings often own those ports)."
    )


def seed_connected_github(workspace_id: str) -> None:
    """Mark github connected so UI Start monitor has a listen source.

    Credentials are empty — poll may set last_error. enabled still becomes true.
    """
    sql_for_workspace(
        workspace_id,
        "INSERT INTO integration_connections ("
        "id, workspace_id, provider, status, account_label, display_name, config, credentials, "
        "connected_at, updated_at, version"
        ") VALUES ("
        f"gen_random_uuid(), {_lit(workspace_id)}, 'github', 'connected', "
        "'inbox-e2e', 'inbox-e2e', '{}'::jsonb, '{}'::jsonb, now(), now(), 1"
        ") ON CONFLICT (workspace_id, provider, display_name) "
        "WHERE display_name <> '' AND status <> 'disconnected' DO UPDATE SET "
        "status = 'connected', last_error = NULL, updated_at = now()",
    )


def seed_open_ticket(workspace_id: str, provider: str, key: str, title: str) -> None:
    """Insert an open unlaunched inbox row so the Finish button is shown."""
    sql_for_workspace(
        workspace_id,
        "INSERT INTO ticket_inbox ("
        "workspace_id, provider, external_key, title, url, state, summary, last_seen_at"
        ") VALUES ("
        f"{_lit(workspace_id)}, {_lit(provider)}, {_lit(key)}, {_lit(title)}, "
        "NULL, 'open', 'inbox e2e seed', now()"
        ") ON CONFLICT (workspace_id, provider, external_key) DO UPDATE SET "
        "title = EXCLUDED.title, summary = EXCLUDED.summary, "
        "state = 'open', run_id = NULL, last_seen_at = now()",
    )
