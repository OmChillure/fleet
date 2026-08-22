#!/usr/bin/env python3
"""Cookie-auth seed helpers for runner catalog browser e2e.

All mutating calls go through the UI origin (`:3030/api/...`) so the
dx proxy keeps `fleet_access` first-party. Machine heartbeat uses
`Authorization: Bearer flm_…` (no session cookie).
"""
from __future__ import annotations

import json
import os
import re
import time
import uuid
from pathlib import Path
from typing import Any

UI = os.environ.get("FLEET_UI_URL", "http://127.0.0.1:3030").rstrip("/")
API = os.environ.get("FLEET_API_URL", "http://127.0.0.1:8080").rstrip("/")
REPO = Path(__file__).resolve().parents[2]


def _json(res) -> dict[str, Any]:
    text = res.text()
    if not res.ok:
        raise RuntimeError(f"{res.status} {res.url}: {text[:500]}")
    if not text:
        return {}
    return json.loads(text)


def signup_login(page, *, prefix: str) -> tuple[str, str]:
    """Create a throwaway owner and land on the workspace dashboard.

    Returns (workspace_id, workspace_url) where workspace_url has no trailing
    slash (e.g. http://127.0.0.1:3030/<uuid>).
    """
    email = f"{prefix}-{uuid.uuid4().hex[:12]}@example.test"
    password = "e2e-pass-ok"
    last_err: Exception | None = None
    for _ in range(24):
        try:
            page.goto(f"{UI}/signup", wait_until="domcontentloaded", timeout=15_000)
            page.locator("#display_name").wait_for(state="attached", timeout=8_000)
            last_err = None
            break
        except Exception as e:  # noqa: BLE001 — dx mid-rebuild; retry
            last_err = e
            time.sleep(2)
    if last_err is not None:
        raise last_err
    # dx "being rebuilt" overlay can cover the form; force still types.
    page.locator("#display_name").fill(f"E2E {prefix}", force=True)
    page.locator("#email").fill(email, force=True)
    page.locator("#password").fill(password, force=True)
    page.get_by_role("button", name="Sign up").click(force=True)
    page.wait_for_url("**/login", timeout=15_000)
    page.locator("#email").fill(email, force=True)
    page.locator("#password").fill(password, force=True)
    page.get_by_role("button", name="Log in").click(force=True)
    page.wait_for_url("**/dashboard", timeout=20_000)
    url = page.url.rsplit("/dashboard", 1)[0]
    ws = url.rstrip("/").rsplit("/", 1)[-1]
    return ws, url


def create_host(page, workspace_id: str, name: str) -> dict[str, Any]:
    return _json(
        page.request.post(
            f"{UI}/api/workspaces/{workspace_id}/worker-hosts",
            data=json.dumps({"name": name}),
            headers={"Content-Type": "application/json"},
        )
    )


def pair_host(
    page,
    pair_token: str,
    *,
    hostname: str,
    platform: str = "linux-x86_64",
) -> dict[str, Any]:
    return _json(
        page.request.post(
            f"{UI}/api/worker-hosts/pair",
            data=json.dumps(
                {
                    "token": pair_token,
                    "hostname": hostname,
                    "platform": platform,
                    "advertised_host": True,
                    "advertised_container": False,
                }
            ),
            headers={"Content-Type": "application/json"},
        )
    )


def heartbeat(page, machine_token: str, extra: dict[str, Any] | None = None) -> dict[str, Any]:
    body: dict[str, Any] = {
        "advertised_host": True,
        "advertised_container": False,
        "current_jobs": 0,
        "max_concurrent": 4,
    }
    if extra:
        body.update(extra)
    return _json(
        page.request.post(
            f"{UI}/api/worker-hosts/me/heartbeat",
            data=json.dumps(body),
            headers={
                "Content-Type": "application/json",
                "Authorization": f"Bearer {machine_token}",
            },
        )
    )


def pair_and_heartbeat(
    page,
    workspace_id: str,
    name: str,
    *,
    platform: str = "linux-x86_64",
    heartbeat_extra: dict[str, Any] | None = None,
) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any]]:
    created = create_host(page, workspace_id, name)
    token = created["pair_token"]
    paired = pair_host(page, token, hostname=name, platform=platform)
    hb = heartbeat(page, paired["machine_token"], heartbeat_extra)
    return created, paired, hb


def patch_host(page, workspace_id: str, host_id: str, body: dict[str, Any]) -> dict[str, Any]:
    return _json(
        page.request.patch(
            f"{UI}/api/workspaces/{workspace_id}/worker-hosts/{host_id}",
            data=json.dumps(body),
            headers={"Content-Type": "application/json"},
        )
    )


def list_hosts(page, workspace_id: str) -> dict[str, Any]:
    return _json(page.request.get(f"{UI}/api/workspaces/{workspace_id}/worker-hosts"))


def wait_dx_ready(page, timeout_ms: int = 20_000) -> None:
    """Wait out dx compile splash / rebuild banner that covers the app."""
    splash = page.get_by_text(
        re.compile(
            r"We're building your app now|Your app is being rebuilt|One sec!",
            re.I,
        )
    )
    deadline = time.time() + (timeout_ms / 1000)
    while time.time() < deadline:
        try:
            busy = splash.count() > 0 and splash.first.is_visible()
        except Exception:
            busy = False
        if not busy:
            return
        page.wait_for_timeout(500)


def goto_self_hosted(page, workspace_url: str) -> None:
    last_err: Exception | None = None
    for _ in range(6):
        try:
            page.goto(
                f"{workspace_url}/runners/self-hosted",
                wait_until="domcontentloaded",
            )
        except Exception as e:  # noqa: BLE001 — dx restart / connection reset
            last_err = e
            time.sleep(2)
            continue
        wait_dx_ready(page)
        try:
            page.wait_for_selector("h1", state="attached", timeout=20_000)
            return
        except Exception as e:  # noqa: BLE001 — dx mid-rebuild; retry
            last_err = e
            time.sleep(1)
    if last_err is not None:
        raise last_err
