#!/usr/bin/env python3
"""Layer 2 e2e: Remove worker Confirm removes; Cancel aborts (catalog:e2e-remove-confirm).

Catalog: Apply / ack (paired) → Remove worker (confirm / cancel).
Cancel restores Remove worker and keeps the card. Confirm revokes; a
never-paired leftover stays hidden (no Show removed). Fail closed — no skip.
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
from runners_e2e_helpers import (  # noqa: E402
    UI,
    create_host,
    goto_self_hosted,
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

CONFIRM_COPY = "Remove this worker?"
CONFIRM_HINT = "It must be paired again to come back."
REMOVED_TOAST = "Worker removed."
SHOW_REMOVED = re.compile(r"^Show removed")


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
    email = f"rm-confirm-{uuid.uuid4().hex[:12]}@example.test"
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
                    "display_name": "E2E rm-confirm",
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


def _plane_has_no_remove(page) -> None:
    n = page.locator("article").filter(has_text="Control plane").count()
    if n:
        raise AssertionError("synthetic Control plane card must not appear")


class CatalogRemoveConfirmTests(unittest.TestCase):
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

    def test_remove_worker_confirm_removes_cancel_aborts(self) -> None:
        # catalog:e2e-remove-confirm — Confirm removes; Cancel aborts.
        page = self.page
        _ws, workspace_url = _signup_api(page)
        name = f"rm-{uuid.uuid4().hex[:8]}"
        create_host(page, _ws, name)
        goto_self_hosted(page, workspace_url)
        wait_dx_ready(page)
        page.wait_for_selector("h1", state="attached", timeout=60_000)

        _plane_has_no_remove(page)

        card = page.locator("article").filter(has_text=name)
        card.wait_for(timeout=10_000)
        self.assertNotIn("Control plane", card.inner_text())
        remove = card.get_by_role("button", name="Remove worker")
        self.assertTrue(remove.is_visible(), "seeded worker must show Remove worker")

        remove.click(force=True)
        page.get_by_text(CONFIRM_COPY).wait_for(timeout=5_000)
        card_text = card.inner_text()
        self.assertIn(CONFIRM_COPY, card_text)
        self.assertIn(CONFIRM_HINT, card_text)
        self.assertTrue(card.get_by_role("button", name="Confirm").is_visible())
        self.assertTrue(card.get_by_role("button", name="Cancel").is_visible())

        card.get_by_role("button", name="Cancel").click(force=True)
        card.get_by_role("button", name="Remove worker").wait_for(timeout=5_000)
        self.assertNotIn(CONFIRM_COPY, card.inner_text())
        self.assertTrue(card.is_visible(), "Cancel must keep the worker card")
        self.assertIn(name, card.inner_text())
        self.assertTrue(
            card.get_by_role("button", name="Remove worker").is_visible(),
            "Cancel must restore Remove worker",
        )
        self.assertEqual(
            card.get_by_role("button", name="Confirm").count(),
            0,
            "Cancel must hide Confirm",
        )

        card.get_by_role("button", name="Remove worker").click(force=True)
        page.get_by_text(CONFIRM_COPY).wait_for(timeout=5_000)
        card.get_by_role("button", name="Confirm").click(force=True)
        card.wait_for(state="detached", timeout=15_000)
        try:
            page.get_by_text(REMOVED_TOAST).wait_for(timeout=8_000)
        except Exception as e:  # noqa: BLE001
            self.fail(
                f"Confirm must toast {REMOVED_TOAST!r} ({e}). "
                f"body={page.inner_text('body')[:1200]!r}"
            )
        self.assertEqual(
            page.locator("article").filter(has_text=name).count(),
            0,
            "Confirm must drop the card from the default list",
        )
        self.assertNotIn(name, page.inner_text("body"))
        self.assertEqual(
            page.get_by_role("button", name=SHOW_REMOVED).count(),
            0,
            "pending leftover must stay hidden (never paired)",
        )

        _plane_has_no_remove(page)

        goto_self_hosted(page, workspace_url)
        wait_dx_ready(page)
        page.wait_for_selector("h1", state="attached", timeout=60_000)
        _plane_has_no_remove(page)
        self.assertEqual(
            page.locator("article").filter(has_text=name).count(),
            0,
            "Confirm must persist across reload",
        )
        self.assertNotIn(name, page.inner_text("body"))
        self.assertEqual(
            page.get_by_role("button", name=SHOW_REMOVED).count(),
            0,
            "never-paired leftover must stay hidden after reload",
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
