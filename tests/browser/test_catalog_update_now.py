#!/usr/bin/env python3
"""Layer 2 e2e: Auto-update off + Update now (catalog:e2e-update-now).

Button shows when the box is behind and auto-update is off; click becomes
Update requested and sets update_requested_at.
"""
from __future__ import annotations

import json
import os
import socket
import sys
import unittest
import uuid
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runners_e2e_helpers import (  # noqa: E402
    UI,
    goto_self_hosted,
    heartbeat,
    list_hosts,
    pair_and_heartbeat,
    patch_host,
)
from stack_identity import ForeignStackError, require_this_tree  # noqa: E402

API = os.environ.get("FLEET_API_URL", "http://127.0.0.1:8080").rstrip("/")
REPO = Path(__file__).resolve().parents[2]
BROWSERS = os.environ.get(
    "PLAYWRIGHT_BROWSERS_PATH",
    str(REPO / ".tmp" / "ms-playwright"),
)
os.environ.setdefault("PLAYWRIGHT_BROWSERS_PATH", BROWSERS)

BEHIND_SHA = "0000000000000000000000000000000000000000"
BEHIND_BINS = {
    "fleet-host": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
    "fleet-driver": "0123456701234567012345670123456701234567012345670123456701234567",
}


def _listening(host: str, port: int) -> bool:
    try:
        with socket.create_connection((host, port), timeout=2):
            return True
    except OSError:
        return False


def _require_stack() -> None:
    try:
        require_this_tree(REPO, API)
    except ForeignStackError as e:
        raise RuntimeError(str(e)) from e
    if not _listening("127.0.0.1", 3030):
        raise RuntimeError(
            "UI :3030 is not listening. Start this tree: ./scripts/human-test.sh launch"
        )


def _require_playwright():
    try:
        from playwright.sync_api import sync_playwright
    except ImportError as e:
        raise RuntimeError(
            "playwright is not installed. Run "
            "crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh "
            "with ENABLE_BROWSER_E2E=1 (also installs Chromium OS libraries)."
        ) from e
    return sync_playwright


def _staged_linux(listed: dict[str, Any]) -> tuple[str, dict[str, str]]:
    sha = ""
    bins: dict[str, str] = {}
    for pkg in listed.get("packages") or []:
        if not isinstance(pkg, dict):
            continue
        if pkg.get("platform") != "linux-x86_64":
            continue
        sha = (pkg.get("git_sha") or "").strip()
        raw = pkg.get("bins") or {}
        if isinstance(raw, dict):
            for stem, val in raw.items():
                if isinstance(val, dict):
                    got = (val.get("sha256") or "").strip()
                else:
                    got = str(val).strip()
                if got:
                    bins[stem] = got
        break
    return sha, bins


def _mismatch_software(listed: dict[str, Any]) -> dict[str, Any]:
    staged_sha, staged_bins = _staged_linux(listed)
    git_sha = BEHIND_SHA
    if staged_sha and git_sha.lower() == staged_sha.lower():
        git_sha = "1111111111111111111111111111111111111111"
    bins = dict(BEHIND_BINS)
    for stem, want in staged_bins.items():
        if stem in bins and bins[stem].lower() == want.lower():
            bins[stem] = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
    return {"git_sha": git_sha, "bins": bins}


def _host_from_hb(hb: dict[str, Any]) -> dict[str, Any]:
    host = hb.get("host")
    return host if isinstance(host, dict) else {}


def _requested_at(host: dict[str, Any]) -> str:
    raw = host.get("update_requested_at")
    return raw.strip() if isinstance(raw, str) else ""


def _get_host(page, workspace_id: str, host_id: str) -> dict[str, Any]:
    res = page.request.get(f"{UI}/api/workspaces/{workspace_id}/worker-hosts/{host_id}")
    if not res.ok:
        raise AssertionError(
            f"GET host {host_id}: {res.status} {res.text()[:400]}"
        )
    body = res.json()
    if not isinstance(body, dict):
        raise AssertionError(f"GET host not an object: {body!r}"[:400])
    return body


def _signup_api(page) -> tuple[str, str]:
    """Cookie session via API so a stuck dx rebuild banner cannot hang signup."""
    email = f"catalog-updnow-{uuid.uuid4().hex[:12]}@example.test"
    password = "e2e-pass-ok"
    headers = {"Content-Type": "application/json"}
    created = page.request.post(
        f"{UI}/api/auth/signup",
        data=json.dumps(
            {
                "email": email,
                "password": password,
                "display_name": "E2E catalog-updnow",
            }
        ),
        headers=headers,
    )
    if created.status not in {200, 201}:
        raise RuntimeError(f"signup {created.status}: {created.text()[:400]}")
    logged = page.request.post(
        f"{UI}/api/auth/login",
        data=json.dumps({"email": email, "password": password}),
        headers=headers,
    )
    if not logged.ok:
        raise RuntimeError(f"login {logged.status}: {logged.text()[:400]}")
    body = logged.json()
    ws = str(((body.get("default_workspace") or {}).get("id")) or "")
    if not ws:
        me = page.request.get(f"{UI}/api/auth/me")
        spaces = (me.json() or {}).get("workspaces") or []
        ws = str((spaces[0].get("id") if spaces else "") or "")
    if not ws:
        raise RuntimeError(f"login missing workspace: {body!r}"[:400])
    return ws, f"{UI}/{ws}"


class CatalogUpdateNowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        _require_stack()
        sync_playwright = _require_playwright()
        cls._pw = sync_playwright().start()
        try:
            cls._browser = cls._pw.chromium.launch(headless=True)
        except Exception as e:
            cls._pw.stop()
            raise RuntimeError(
                f"Chromium launch failed ({e}). Need OS libraries "
                "(nss, atk, gtk, gbm, asound, …) and a browser under "
                ".tmp/ms-playwright. Re-run install-full-suite-deps.sh "
                "with ENABLE_BROWSER_E2E=1 in repo-root .env."
            ) from e
        cls._context = cls._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        cls.page = cls._context.new_page()
        cls.page.set_default_timeout(30_000)

    @classmethod
    def tearDownClass(cls) -> None:
        cls._context.close()
        cls._browser.close()
        cls._pw.stop()

    def test_update_now_when_behind_becomes_update_requested(self) -> None:
        from playwright.sync_api import expect

        page = self.page
        ws, workspace_url = _signup_api(page)
        name = f"updnow-box-{uuid.uuid4().hex[:12]}"

        listed = list_hosts(page, ws)
        staged_sha, staged_bins = _staged_linux(listed)
        self.assertTrue(
            staged_sha or staged_bins,
            "staged linux-x86_64 package is required to mark a host behind "
            "(scripts/stage-worker-dist.sh). Missing staged dist is a fail.",
        )
        created, paired, hb = pair_and_heartbeat(
            page,
            ws,
            name,
            heartbeat_extra={"software": _mismatch_software(listed)},
        )
        host = _host_from_hb(hb)
        self.assertTrue(
            bool(host.get("outdated")) or host.get("software_status") == "behind",
            "API did not mark the host behind/outdated "
            f"(software_status={host.get('software_status')!r} "
            f"outdated={host.get('outdated')!r} desired={host.get('desired')!r})",
        )

        host_id = created["host"]["id"]
        patched = patch_host(page, ws, host_id, {"auto_update": False})
        self.assertFalse(
            patched.get("auto_update", True),
            f"PATCH auto_update=false: {patched}",
        )
        self.assertFalse(
            _requested_at(patched),
            f"turning auto-update off must not request an update: {patched}",
        )
        gen = patched.get("settings_gen")
        self.assertIsNotNone(gen, f"PATCH auto_update must return settings_gen: {patched}")
        heartbeat(
            page,
            paired["machine_token"],
            {"settings_ack_gen": int(gen)},
        )

        goto_self_hosted(page, workspace_url)
        expect(page.get_by_role("button", name="Add worker")).to_be_visible()
        expect(page.locator("article").filter(has_text="Control plane")).to_have_count(0)

        card = page.locator("article").filter(has_text=name)
        expect(card).to_be_visible()
        update_now = card.get_by_role("button", name="Update now")
        expect(update_now).to_be_visible()
        update_now.click()
        expect(card.get_by_role("button", name="Update requested")).to_be_visible()

        got = _get_host(page, ws, host_id)
        self.assertTrue(
            _requested_at(got),
            f"GET update_requested_at must be set after Update now: {got}",
        )
        self.assertFalse(
            got.get("auto_update", True),
            f"Update now must not flip auto-update: {got}",
        )


if __name__ == "__main__":
    unittest.main()
