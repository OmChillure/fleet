#!/usr/bin/env python3
"""Layer 2 e2e: Show removed after revoke of a paired box (catalog:e2e-show-removed).

Never-paired leftovers stay hidden. A revoked paired box is hidden by default
and appears under Show removed (N) with a removed pill. Hide removed (N)
collapses it again.
"""
from __future__ import annotations

import json
import os
import re
import socket
import sys
import unittest
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runners_e2e_helpers import (  # noqa: E402
    UI,
    create_host,
    goto_self_hosted,
    pair_and_heartbeat,
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

SHOW_REMOVED = re.compile(r"^Show removed \(\d+\)$")
HIDE_REMOVED = re.compile(r"^Hide removed \(\d+\)$")
CONFIRM_COPY = "Remove this worker?"


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


def _api_workspace(page) -> tuple[str, str]:
    """Cookie session via API so signup WASM/rebuild cannot hide the form."""
    email = f"show-rm-{uuid.uuid4().hex[:12]}@example.test"
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
                    "display_name": "E2E show-rm",
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
    ws = ((body.get("default_workspace") or {}).get("id")) or ""
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


class CatalogShowRemovedTests(unittest.TestCase):
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

    def test_show_removed_after_revoke_of_a_paired_box(self) -> None:
        page = self.page
        ws, workspace_url = _api_workspace(page)
        leftover = f"never-paired-{uuid.uuid4().hex[:8]}"
        paired_name = f"paired-rm-{uuid.uuid4().hex[:8]}"

        leftover_created = create_host(page, ws, leftover)
        leftover_id = leftover_created["host"]["id"]
        revoked = page.request.delete(
            f"{UI}/api/workspaces/{ws}/worker-hosts/{leftover_id}"
        )
        self.assertTrue(
            revoked.ok,
            f"revoke leftover {leftover_id}: {revoked.status} {revoked.text()[:300]}",
        )

        pair_and_heartbeat(page, ws, paired_name)
        goto_self_hosted(page, workspace_url)
        wait_dx_ready(page)
        page.wait_for_selector("h1", state="attached", timeout=60_000)

        card = page.locator("article").filter(has_text=paired_name)
        card.first.wait_for(state="attached", timeout=30_000)
        self.assertNotIn(
            leftover,
            page.inner_text("body"),
            "revoked never-paired leftover must stay hidden",
        )
        self.assertEqual(
            page.get_by_role("button", name=SHOW_REMOVED).count(),
            0,
            "Show removed must stay hidden until a paired box is revoked",
        )

        card.get_by_role("button", name="Remove worker").click(force=True)
        page.get_by_text(CONFIRM_COPY).wait_for(timeout=5_000)
        card.get_by_role("button", name="Confirm").click(force=True)
        card.wait_for(state="detached", timeout=15_000)

        body = page.inner_text("body")
        self.assertNotIn(
            paired_name,
            body,
            "removed paired box must be hidden by default",
        )
        self.assertNotIn(
            leftover,
            body,
            "never-paired leftover must stay hidden after paired revoke",
        )

        show = page.get_by_role("button", name=SHOW_REMOVED)
        show.wait_for(timeout=5_000)
        self.assertTrue(
            show.is_visible(),
            "Show removed (N) after revoke of a paired box",
        )
        self.assertRegex(show.inner_text().strip(), r"^Show removed \([1-9]\d*\)$")
        show.click(force=True)

        page.get_by_role("button", name=HIDE_REMOVED).wait_for(timeout=5_000)
        restored = page.locator("article").filter(has_text=paired_name)
        restored.wait_for(timeout=10_000)
        self.assertTrue(restored.is_visible(), "Show removed must reveal the paired box")
        restored_text = restored.inner_text()
        self.assertRegex(restored_text.lower(), r"\bremoved\b")
        self.assertNotIn("Control plane", restored_text)
        self.assertEqual(
            page.locator("article").filter(has_text=leftover).count(),
            0,
            "never-paired leftover must stay hidden after Show removed",
        )
        self.assertNotIn(leftover, page.inner_text("body"))

        hide = page.get_by_role("button", name=HIDE_REMOVED)
        hide.click(force=True)
        page.get_by_role("button", name=SHOW_REMOVED).wait_for(timeout=5_000)
        restored.wait_for(state="detached", timeout=10_000)
        self.assertEqual(
            page.locator("article").filter(has_text=paired_name).count(),
            0,
            "Hide removed must collapse the revoked paired box",
        )
        self.assertNotIn(paired_name, page.inner_text("body"))
        self.assertNotIn(leftover, page.inner_text("body"))


if __name__ == "__main__":
    unittest.main(verbosity=2)
