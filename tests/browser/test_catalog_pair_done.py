#!/usr/bin/env python3
"""Layer 2 e2e: Done keeps the pending worker (catalog:e2e-pair-done).

Done hides the pair panel. The unused pending card stays on the list
and is still pending after reload. Must not revoke/DELETE (Cancel is
list-pair-cancel-http-discards-pending). Fail closed.
"""
from __future__ import annotations

import json
import os
import socket
import sys
import time
import unittest
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runners_e2e_helpers import (  # noqa: E402
    UI,
    goto_self_hosted,
    list_hosts,
    wait_dx_ready,
)
from stack_identity import ForeignStackError, require_this_tree  # noqa: E402

API = os.environ.get("FLEET_API_URL", "http://127.0.0.1:8080").rstrip("/")
REPO = Path(__file__).resolve().parents[2]
BROWSERS = os.environ.get(
    "PLAYWRIGHT_BROWSERS_PATH",
    str(REPO / ".tmp" / "ms-playwright"),
)
os.environ.setdefault("PLAYWRIGHT_BROWSERS_PATH", BROWSERS)

DELETE_HOST = "**/api/workspaces/*/worker-hosts/*"


def _listening(host: str, port: int) -> bool:
    try:
        with socket.create_connection((host, port), timeout=2):
            return True
    except OSError:
        return False


def _require_stack() -> None:
    last: Exception | None = None
    for _ in range(12):
        try:
            require_this_tree(REPO, API)
            if _listening("127.0.0.1", 3030):
                return
            last = RuntimeError(
                "UI :3030 is not listening. Start this tree: ./scripts/human-test.sh launch"
            )
        except ForeignStackError as e:
            last = e
        time.sleep(1)
    if isinstance(last, ForeignStackError):
        raise RuntimeError(str(last)) from last
    raise RuntimeError(str(last))


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
    email = f"pair-done-{uuid.uuid4().hex[:12]}@example.test"
    password = "e2e-pass-ok"
    headers = {"Content-Type": "application/json"}
    created = None
    last_err = ""
    for _ in range(12):
        created = page.request.post(
            f"{UI}/api/auth/signup",
            data=json.dumps(
                {
                    "email": email,
                    "password": password,
                    "display_name": "E2E pair-done",
                }
            ),
            headers=headers,
        )
        if created.status in {200, 201}:
            break
        last_err = f"signup {created.status}: {created.text()[:400]}"
        page.wait_for_timeout(750)
    if created is None or created.status not in {200, 201}:
        raise RuntimeError(last_err or "signup failed")
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
        if not me.ok:
            raise RuntimeError(f"me {me.status}: {me.text()[:400]}")
        spaces = (me.json() or {}).get("workspaces") or []
        if spaces:
            ws = str(spaces[0].get("id") or "")
    if not ws:
        raise RuntimeError(f"login missing workspace: {body!r}"[:400])
    return ws, f"{UI}/{ws}"


class CatalogPairDoneTests(unittest.TestCase):
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
        cls.page.set_default_timeout(20_000)

    @classmethod
    def tearDownClass(cls) -> None:
        cls._context.close()
        cls._browser.close()
        cls._pw.stop()

    def test_done_hides_panel_keeps_pending_after_reload(self) -> None:
        # catalog:e2e-pair-done — Done hides panel; list button still pending.
        page = self.page
        ws, workspace_url = _signup_api(page)
        goto_self_hosted(page, workspace_url)
        wait_dx_ready(page)
        deletes: list[str] = []

        def _capture_delete(route):
            if route.request.method.upper() == "DELETE":
                deletes.append(route.request.url)
            route.continue_()

        page.route(DELETE_HOST, _capture_delete)
        try:
            heading = page.get_by_role("heading", name="Pair this worker")
            page.get_by_role("button", name="Add worker").click()
            heading.wait_for(timeout=20_000)

            listed = list_hosts(page, ws)
            created = [h for h in (listed.get("hosts") or []) if h.get("id")]
            self.assertEqual(
                len(created),
                1,
                f"Add worker must create exactly one pending host: {listed!r}"[:800],
            )
            hid = str(created[0]["id"])

            page.get_by_role("button", name="Done", exact=True).click()
            heading.wait_for(state="hidden", timeout=10_000)
            self.assertEqual(
                deletes,
                [],
                f"Done must not DELETE/revoke the unused pending host: {deletes}",
            )

            card = page.get_by_role("button").filter(has_text="pending")
            card.first.wait_for(timeout=10_000)
            self.assertIn("pending", card.first.inner_text().lower())

            page.reload(wait_until="domcontentloaded")
            wait_dx_ready(page)
            page.get_by_role("button", name="Add worker").wait_for(timeout=20_000)

            detail = page.request.get(f"{UI}/api/workspaces/{ws}/worker-hosts/{hid}")
            self.assertTrue(
                detail.ok,
                f"GET host after Done+reload: {detail.status} {detail.text()[:400]}",
            )
            body = detail.json()
            self.assertEqual(body.get("status"), "pending", body)
            self.assertFalse(body.get("paired_at"), body)
            self.assertEqual(
                deletes,
                [],
                f"reload must not DELETE the unused pending host: {deletes}",
            )
        finally:
            page.unroute(DELETE_HOST, _capture_delete)
