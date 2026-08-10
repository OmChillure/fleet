#!/usr/bin/env python3
"""Fail closed unless this checkout's tools and/or live stack are present.

Port-up is not this tree. Kind extraPortMappings steal host :8080/:3030.

  python3 tests/browser/stack_identity.py          # live API + UI must match HEAD
  python3 tests/browser/stack_identity.py --tools  # cargo/dx/docker/… must be on PATH
  python3 tests/browser/stack_identity.py --all    # tools then live stack

There is no green path for a foreign stack, a missing dx, or a faked
`FLEET_GIT_SHA` env. Rebuild images / run the install script.

Used by human-test.sh, fleet-dev.sh, record_demo.py, test_app_e2e.py,
run-every-test.sh, kind smoke.
"""
from __future__ import annotations

import json
import os
import re
import shutil
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
UI_SHA_PATH = "/fleet-git-sha.txt"
REQUIRED_TOOLS = ("git", "cargo", "rustc", "python3", "docker", "dx")
SHA_RE = re.compile(r"^[0-9a-f]{40}(-[0-9a-f]{12})?$")
INSTALL = (
    "crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh"
)
TREE_ID = REPO / "scripts" / "tree-id.sh"

FOREIGN_HINT = (
    "This is not this worktree (Kind extraPortMappings often own :8080/:3030 "
    "with old agent-fleet-api:local / agent-fleet-ui:local). Port-up is not "
    "our stack. After any code change, run the install script or "
    "./scripts/run-every-test.sh --prepare-kind (bakes scripts/tree-id.sh). "
    "Verification without that rebuild is not valid. "
    "Do not kubectl set env FLEET_GIT_SHA — the API sha is baked at compile. "
    "Do not treat Kind UI as a substitute for host dx."
)


class ForeignStackError(RuntimeError):
    pass


class MissingToolsError(RuntimeError):
    pass


def expected_sha(repo: Path | None = None) -> str:
    root = repo or REPO
    script = (root / "scripts" / "tree-id.sh") if root != REPO else TREE_ID
    out = subprocess.check_output(
        ["bash", str(script)],
        text=True,
        timeout=30,
        cwd=str(root),
    )
    return out.strip()


def listening(host: str, port: int) -> bool:
    try:
        with socket.create_connection((host, port), timeout=2):
            return True
    except OSError:
        return False


def require_tools() -> None:
    missing = [name for name in REQUIRED_TOOLS if shutil.which(name) is None]
    if missing:
        raise MissingToolsError(
            "MISSING_TOOLS: "
            + ", ".join(missing)
            + f". Run {INSTALL} (no arguments; PROFILE/ENABLE_* in repo-root .env). "
            "Do not WARN-and-continue. Do not use Kind UI as a substitute for host dx."
        )


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


def probe_ui_sha() -> str:
    url = f"http://{UI_HOST}:{UI_PORT}{UI_SHA_PATH}"
    try:
        with urllib.request.urlopen(url, timeout=3) as resp:
            raw = resp.read().decode()
            status = resp.status
            ctype = resp.headers.get("Content-Type") or ""
    except (urllib.error.URLError, TimeoutError, OSError) as e:
        raise ForeignStackError(
            f"UI {url} is down or missing ({e}). Old UI images have no "
            f"{UI_SHA_PATH}. Rebuild agent-fleet-ui:local from this commit "
            "or start host dx via ./scripts/human-test.sh launch. {0}".format(
                FOREIGN_HINT
            )
        ) from e
    text = raw.strip()
    if status != 200:
        raise ForeignStackError(f"{url} HTTP {status}: {raw[:200]!r}")
    if "html" in ctype.lower() or text[:15].lower().startswith(
        ("<!doctype", "<html")
    ):
        raise ForeignStackError(
            f"{url} returned HTML (SPA fallback / old UI without the sha file). "
            + FOREIGN_HINT
        )
    if not SHA_RE.fullmatch(text):
        raise ForeignStackError(f"{url} is not a 40-char git sha: {text[:80]!r}")
    return text


def require_this_tree(
    repo: Path | None = None,
    api_url: str = API_DEFAULT,
    require_ui: bool = True,
) -> dict:
    want = expected_sha(repo)
    body = probe(api_url)

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
            f"/health.git_sha is null/missing (old binary or not baked at compile). "
            f"{FOREIGN_HINT}"
        )
    elif sha != want:
        problems.append(
            f"/health.git_sha={sha} != this tree HEAD {want}. {FOREIGN_HINT}"
        )
    if "chat" not in apis:
        problems.append(
            f"/health.apis missing 'chat' (old fleet-server): {apis!r}. {FOREIGN_HINT}"
        )
    if require_ui:
        if not listening(UI_HOST, UI_PORT):
            problems.append(
                "UI :3030 is down. Start this tree: ./scripts/human-test.sh launch "
                "(needs host dx from the install script)."
            )
        else:
            try:
                ui_sha = probe_ui_sha()
            except ForeignStackError as e:
                problems.append(str(e))
            else:
                if ui_sha != want:
                    problems.append(
                        f"UI {UI_SHA_PATH}={ui_sha} != this tree HEAD {want}. "
                        f"{FOREIGN_HINT}"
                    )
                body = dict(body)
                body["ui_git_sha"] = ui_sha

    if problems:
        raise ForeignStackError(" ".join(problems))
    return body


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    want_tools = "--tools" in args or "--all" in args
    want_stack = "--all" in args or not want_tools or "--stack" in args
    if "--tools" in args and "--all" not in args and "--stack" not in args:
        want_stack = False
    require_ui = "--api-only" not in args
    try:
        if want_tools:
            require_tools()
            print("TOOLS_OK " + " ".join(REQUIRED_TOOLS))
        if want_stack:
            body = require_this_tree(require_ui=require_ui)
            print(
                f"THIS_TREE git_sha={body.get('git_sha')} "
                f"ui_git_sha={body.get('ui_git_sha')} apis={body.get('apis')}"
            )
    except MissingToolsError as e:
        print(str(e), file=sys.stderr)
        return 2
    except ForeignStackError as e:
        print(f"FOREIGN_STACK: {e}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
