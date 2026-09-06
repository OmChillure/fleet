#!/usr/bin/env python3
"""Layer 2 e2e: Cloud cards do not create hosts (catalog:list-cloud-four-provider-cards).

Clicking AWS / GCP / Azure / Fleet Cloud on /runners/cloud must not mint a
worker-hosts row. Four-card / no-button chrome is test_11_cloud_page_coming_soon.
"""
from __future__ import annotations

import json
import os
import re
import socket
import sys
import time
import unittest
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runners_e2e_helpers import UI, list_hosts, wait_dx_ready  # noqa: E402
from stack_identity import ForeignStackError, require_this_tree  # noqa: E402

API = os.environ.get("FLEET_API_URL", "http://127.0.0.1:8080").rstrip("/")
REPO = Path(__file__).resolve().parents[2]
BROWSERS = os.environ.get(
    "PLAYWRIGHT_BROWSERS_PATH",
    str(REPO / ".tmp" / "ms-playwright"),
)
os.environ.setdefault("PLAYWRIGHT_BROWSERS_PATH", BROWSERS)

CLOUD_NAMES = ("AWS", "GCP", "Azure", "Fleet Cloud")
_CLOUD_URL = re.compile(r".*/[0-9a-f-]{36}/runners/cloud(?:[?#].*)?$")
_WORKER_HOSTS_WRITE_RE = re.compile(r"/worker-hosts(?:/|$|\?)", re.I)


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


def _signup_api(page) -> tuple[str, str]:
    """Cookie session via API so a stuck dx rebuild banner cannot hang signup."""
    email = f"catalog-cloud-{uuid.uuid4().hex[:12]}@example.test"
    password = "e2e-pass-ok"
    headers = {"Content-Type": "application/json"}
    created = None
    last = ""
    for _ in range(12):
        created = page.request.post(
            f"{UI}/api/auth/signup",
            data=json.dumps(
                {
                    "email": email,
                    "password": password,
                    "display_name": "E2E catalog-cloud",
                }
            ),
            headers=headers,
        )
        if created.status in {200, 201}:
            break
        last = f"signup {created.status}: {created.text()[:400]}"
        time.sleep(0.75)
    if created is None or created.status not in {200, 201}:
        raise RuntimeError(last or "signup failed")
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


def _goto_cloud(page, base: str) -> None:
    last_err: Exception | None = None
    for _ in range(24):
        try:
            page.goto(
                f"{base}/runners/cloud",
                wait_until="domcontentloaded",
                timeout=15_000,
            )
            wait_dx_ready(page)
            page.locator("h1").wait_for(state="visible", timeout=8_000)
            if not re.search(r"/[0-9a-f-]{36}/runners/cloud", page.url):
                raise RuntimeError(f"not on cloud: {page.url}")
            last_err = None
            break
        except Exception as e:  # noqa: BLE001 — dx mid-rebuild; retry
            last_err = e
            time.sleep(2)
    if last_err is not None:
        raise last_err


def _own_host_ids(payload: dict, workspace_id: str) -> list[str]:
    """Workspace-owned worker-hosts ids. Plane / foreign cluster cards are skipped."""
    rows = payload.get("hosts") if isinstance(payload, dict) else payload
    if not isinstance(rows, list):
        rows = []
    ids: list[str] = []
    for host in rows:
        if not isinstance(host, dict):
            continue
        if str(host.get("workspace_id") or "") != workspace_id:
            continue
        hid = str(host.get("id") or "")
        if hid:
            ids.append(hid)
    return ids


class CatalogCloudComingSoonE2ETests(unittest.TestCase):
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

    @classmethod
    def tearDownClass(cls) -> None:
        cls._browser.close()
        cls._pw.stop()

    def test_cloud_four_provider_cards_picker_does_not_create_hosts(self) -> None:
        """catalog:list-cloud-four-provider-cards

        Clicking AWS / GCP / Azure / Fleet Cloud must not create a worker-hosts
        row. A silent native or kind=cloud POST is a fail.
        """
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            ws, base = _signup_api(page)
            before = _own_host_ids(list_hosts(page, ws), ws)
            self.assertEqual(
                before,
                [],
                "fresh workspace must have no worker-hosts rows before the Cloud visit",
            )

            writes: list[str] = []

            def on_request(req) -> None:
                if req.method.upper() not in {"POST", "PUT", "PATCH", "DELETE"}:
                    return
                if _WORKER_HOSTS_WRITE_RE.search(req.url):
                    writes.append(f"{req.method} {req.url}")

            page.on("request", on_request)
            try:
                _goto_cloud(page, base)
                for name in CLOUD_NAMES:
                    page.get_by_role("heading", name=name, exact=True).click()
                    page.wait_for_url(_CLOUD_URL)
            finally:
                page.remove_listener("request", on_request)

            self.assertEqual(
                writes,
                [],
                "clicking Cloud provider cards must not write worker-hosts",
            )
            after = _own_host_ids(list_hosts(page, ws), ws)
            self.assertEqual(
                after,
                [],
                "clicking Cloud cards must not create a worker-hosts row: "
                f"{after!r}",
            )
        finally:
            ctx.close()


if __name__ == "__main__":
    unittest.main(verbosity=2)
