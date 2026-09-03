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


def _host_ids(page, workspace_id: str) -> set[str]:
    listed = list_hosts(page, workspace_id)
    ids: set[str] = set()
    for host in listed.get("hosts") or []:
        hid = str((host or {}).get("id") or "")
        if hid:
            ids.add(hid)
    return ids


def _host_row(page, workspace_id: str, host_id: str) -> dict:
    listed = list_hosts(page, workspace_id)
    for host in listed.get("hosts") or []:
        if host.get("id") == host_id:
            return host
    raise AssertionError(f"host {host_id} missing from list: {listed!r}"[:800])


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
        # catalog:e2e-pair-done — Done hides panel; card still pending.
        page = self.page
        ws, workspace_url = _signup_api(page)
        goto_self_hosted(page, workspace_url)
        wait_dx_ready(page)
        page.get_by_text("Control plane", exact=True).first.wait_for(
            state="attached", timeout=90_000
        )
        before_ids = _host_ids(page, ws)
        deletes: list[str] = []

        def _capture_delete(route):
            if route.request.method.upper() == "DELETE":
                deletes.append(route.request.url)
            route.continue_()

        page.route(DELETE_HOST, _capture_delete)
        try:
            add = page.get_by_role("button", name="Add worker")
            deadline = time.time() + 180
            last_body = ""
            last_err: Exception | None = None
            opened = False
            while time.time() < deadline:
                try:
                    last_body = page.inner_text("body")
                    if add.count() and add.first.is_visible():
                        add.first.click(force=True)
                        page.get_by_role("heading", name="Pair this worker").wait_for(
                            timeout=20_000
                        )
                        opened = True
                        break
                except Exception as e:  # noqa: BLE001 — dx overlay
                    last_err = e
                page.wait_for_timeout(500)
            if not opened:
                raise AssertionError(
                    "Add worker / Pair this worker missing (dx rebuild overlay?). "
                    f"url={page.url} body={last_body[:1500]!r} err={last_err}"
                )

            token = page.locator("pre").filter(has_text="flp_").last.inner_text().strip()
            self.assertTrue(token.startswith("flp_"), token)
            after_ids = _host_ids(page, ws) - before_ids
            self.assertEqual(
                len(after_ids),
                1,
                f"Add worker must create exactly one pending host: {after_ids}",
            )
            hid = next(iter(after_ids))
            row = _host_row(page, ws, hid)
            self.assertEqual(row.get("status"), "pending", row)
            self.assertFalse(row.get("paired_at"), row)

            page.get_by_role("button", name="Done", exact=True).click()
            page.get_by_role("heading", name="Pair this worker").wait_for(
                state="hidden", timeout=10_000
            )
            self.assertEqual(
                deletes,
                [],
                f"Done must not DELETE/revoke the unused pending host: {deletes}",
            )
            self.assertNotIn(token, page.inner_text("body"))
            self.assertEqual(
                page.get_by_role("heading", name="Pair this worker").count(),
                0,
                "Done must close the pair panel",
            )

            card = page.locator("article").filter(has_text="Waiting to pair")
            card.first.wait_for(timeout=10_000)
            text = card.first.inner_text()
            self.assertIn("pending", text.lower())
            self.assertIn("Waiting to pair", text)
            self.assertNotIn("removed", text.lower())

            still = _host_row(page, ws, hid)
            self.assertEqual(still.get("status"), "pending", still)
            self.assertFalse(still.get("paired_at"), still)

            page.reload(wait_until="domcontentloaded")
            wait_dx_ready(page)
            page.wait_for_selector("h1", state="attached", timeout=60_000)
            page.get_by_text("Control plane", exact=True).first.wait_for(
                state="attached", timeout=90_000
            )
            self.assertEqual(
                page.get_by_role("heading", name="Pair this worker").count(),
                0,
                "reload must not reopen the pair panel",
            )
            self.assertNotIn(token, page.inner_text("body"))
            reloaded = page.locator("article").filter(has_text="Waiting to pair")
            reloaded.first.wait_for(timeout=10_000)
            reload_text = reloaded.first.inner_text()
            self.assertIn("pending", reload_text.lower())
            self.assertIn("Waiting to pair", reload_text)

            after_reload = _host_row(page, ws, hid)
            self.assertEqual(after_reload.get("status"), "pending", after_reload)
            self.assertFalse(after_reload.get("paired_at"), after_reload)
            self.assertNotEqual(
                after_reload.get("status"),
                "disabled",
                f"Done must not revoke: {after_reload}",
            )
            detail = page.request.get(f"{UI}/api/workspaces/{ws}/worker-hosts/{hid}")
            self.assertTrue(
                detail.ok, f"GET host after Done+reload: {detail.status} {detail.text()[:400]}"
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
