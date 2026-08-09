#!/usr/bin/env python3
"""Fail closed unless live /health is *this* git tree.

Kind extraPortMappings steal host :8080/:3030. Port-up is not enough.

  python3 tests/browser/stack_identity.py
  FLEET_ALLOW_FOREIGN_STACK=1 python3 tests/browser/stack_identity.py

Used by human-test.sh, fleet-dev.sh, record_demo.py, test_app_e2e.py, kind smoke.
"""
from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
API_DEFAULT = os.environ.get("FLEET_API_URL", "http://127.0.0.1:8080").rstrip("/")
UI_HOST = "127.0.0.1"
UI_PORT = 3030

FOREIGN_HINT = (
    "This is not this checkout's fleet-server (Kind extraPortMappings often "
    "own :8080/:3030 with old agent-fleet-api:local / agent-fleet-ui:local). "
    "Do not treat 'port is up' as our stack. Free those ports or rebuild "
    "images from this commit with --build-arg FLEET_GIT_SHA=$(git rev-parse HEAD). "
    "Then: ./scripts/human-test.sh launch"
)


class ForeignStackError(RuntimeError):
    pass


def expected_sha(repo: Path | None = None) -> str:
    root = repo or REPO
    out = subprocess.check_output(
        ["git", "-C", str(root), "rev-parse", "HEAD"],
        text=True,
        timeout=10,
    )
    return out.strip()


def listening(host: str, port: int) -> bool:
    try:
        with socket.create_connection((host, port), timeout=2):
            return True
    except OSError:
        return False


def probe(api_url: str = API_DEFAULT) -> dict:
    url = api_url.rstrip("/") + "/health"
    try:
        with urllib.request.urlopen(url, timeout=3) as resp:
            raw = resp.read().decode()
            status = resp.status
    except (urllib.error.URLError, TimeoutError, OSError) as e:
        raise ForeignStackError(
            f"API {url} is down ({e}). Start this tree: "
            "./scripts/human-test.sh launch (or ./scripts/fleet-dev.sh start)."
        ) from e
    try:
        body = json.loads(raw)
    except json.JSONDecodeError as e:
        raise ForeignStackError(f"{url} is not JSON health: {raw[:200]!r}") from e
    if status != 200:
        raise ForeignStackError(f"{url} HTTP {status}: {raw[:200]}")
    return body if isinstance(body, dict) else {}


def require_this_tree(
    repo: Path | None = None,
    api_url: str = API_DEFAULT,
    require_ui: bool = True,
) -> dict:
    allow = os.environ.get("FLEET_ALLOW_FOREIGN_STACK", "") == "1"
    want = expected_sha(repo)
    try:
        body = probe(api_url)
    except ForeignStackError:
        if allow:
            print(
                "WARN: FLEET_ALLOW_FOREIGN_STACK=1 and API probe failed; continuing.",
                file=sys.stderr,
            )
            return {}
        raise

    sha = body.get("git_sha")
    if isinstance(sha, str):
        sha = sha.strip() or None
    else:
        sha = None
    apis = body.get("apis") or []
    if not isinstance(apis, list):
        apis = []

    problems: list[str] = []
    if sha is None:
        problems.append(
            f"/health.git_sha is null/missing (old image or FLEET_GIT_SHA unset). {FOREIGN_HINT}"
        )
    elif sha != want:
        problems.append(
            f"/health.git_sha={sha} != this tree HEAD {want}. {FOREIGN_HINT}"
        )
    if "chat" not in apis:
        problems.append(
            f"/health.apis missing 'chat' (old fleet-server): {apis!r}. {FOREIGN_HINT}"
        )
    if require_ui and not listening(UI_HOST, UI_PORT):
        problems.append(
            "UI :3030 is down. Start this tree: ./scripts/human-test.sh launch "
            "(needs dx)."
        )

    if problems:
        msg = " ".join(problems)
        if allow:
            print(f"WARN: FLEET_ALLOW_FOREIGN_STACK=1 — {msg}", file=sys.stderr)
            return body
        raise ForeignStackError(msg)
    return body


def main(argv: list[str] | None = None) -> int:
    del argv  # unused
    try:
        body = require_this_tree()
    except ForeignStackError as e:
        print(f"FOREIGN_STACK: {e}", file=sys.stderr)
        return 2
    print(f"THIS_TREE git_sha={body.get('git_sha')} apis={body.get('apis')}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
