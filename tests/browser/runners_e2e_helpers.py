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
    try:
        page.wait_for_url("**/login", timeout=15_000)
    except Exception as nav_err:
        body = ""
        try:
            body = page.locator("body").inner_text(timeout=2_000)
        except Exception:  # noqa: BLE001 — best-effort toast/body
            body = page.url
        lowered = body.lower()
        if "too many attempts" in lowered or "try again later" in lowered:
            raise RuntimeError(f"signup rate-limited: {body[:500]}") from nav_err
        raise
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
    advertised_host: bool = True,
    advertised_container: bool = False,
) -> dict[str, Any]:
    return _json(
        page.request.post(
            f"{UI}/api/worker-hosts/pair",
            data=json.dumps(
                {
                    "token": pair_token,
                    "hostname": hostname,
                    "platform": platform,
                    "advertised_host": advertised_host,
                    "advertised_container": advertised_container,
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
    """Wait out a real dx compile splash.

    A leftover "Your app is being rebuilt" toast can sit on a live WASM
    app. If Self-hosted chrome is already attached, do not block.
    """
    splash = page.get_by_text(
        re.compile(
            r"We're building your app now|Your app is being rebuilt|One sec!",
            re.I,
        )
    )
    deadline = time.time() + (timeout_ms / 1000)
    while time.time() < deadline:
        try:
            chrome = page.get_by_role("button", name="Add worker").count() > 0
        except Exception:
            chrome = False
        if chrome:
            return
        try:
            busy = splash.count() > 0 and splash.first.is_visible()
        except Exception:
            busy = False
        if not busy:
            return
        page.wait_for_timeout(500)


def click_add_worker(page) -> None:
    """Click Add worker. Name is optional; pair fills the device hostname."""
    page.get_by_role("button", name="Add worker").click()


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
            page.get_by_role("button", name="Add worker").wait_for(timeout=10_000)
            # First list fetch shows Loading…; extras (this-machine) appear after.
            deadline = time.time() + 15
            while time.time() < deadline:
                try:
                    if page.get_by_text("Loading…").count() == 0:
                        break
                    if not page.get_by_text("Loading…").first.is_visible():
                        break
                except Exception:
                    break
                page.wait_for_timeout(200)
            return
        except Exception as e:  # noqa: BLE001 — dx mid-rebuild; retry
            last_err = e
            time.sleep(1)
    if last_err is not None:
        raise last_err


def host_list_row(page, needle: str):
    """Catalog list rows are buttons; the side panel is the article."""
    return page.get_by_role("button").filter(has_text=needle)


def open_host_panel(page, needle: str, timeout_ms: int = 15_000):
    """Open HostDetailPanel (article). List rows do not include
    ``Waiting to pair`` — that sentence is detail-only."""
    wait_dx_ready(page)
    art = page.locator("article").filter(has_text=needle)
    try:
        if art.count() > 0 and art.first.is_visible():
            return art
    except Exception:
        pass
    row = host_list_row(page, needle)
    if row.count() == 0 and needle == "Waiting to pair":
        row = host_list_row(page, "Unnamed worker")
        if row.count() == 0:
            row = host_list_row(page, "pending")
    row.first.wait_for(state="attached", timeout=timeout_ms)
    row.first.click()
    art = page.locator("article").filter(has_text=needle)
    if art.count() == 0:
        art = page.locator("article")
    art.first.wait_for(timeout=timeout_ms)
    return art


def assert_no_control_plane_card(page) -> None:
    """The API process is not a runner.

    A paired laptop titled ``name (control plane · this machine)`` is a
    real host card, not the removed synthetic plane row.
    """
    exact = re.compile(r"^Control plane\s*$")
    n = page.locator("article").filter(has_text=exact).count()
    if n:
        raise AssertionError(
            f"synthetic Control plane card must not appear ({n}). "
            f"body={page.inner_text('body')[:400]!r}"
        )


def assert_hosts_have_no_plane(listed: dict[str, Any]) -> None:
    for h in listed.get("hosts") or []:
        if h.get("kind") == "control_plane":
            raise AssertionError(f"list must not include a plane host: {h!r}"[:800])
