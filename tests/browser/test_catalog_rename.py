#!/usr/bin/env python3
"""Layer 2 e2e: rename a Self-hosted card (Enter saves, Escape cancels).

Catalog: List, identity, status → Rename (pencil, Enter / Escape).
"""
from __future__ import annotations

import os
import socket
import sys
import unittest
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runners_e2e_helpers import (  # noqa: E402
    assert_no_control_plane_card,
    create_host,
    goto_self_hosted,
    open_host_panel,
    signup_login,
    wait_dx_ready,
)
from stack_identity import ForeignStackError, require_this_tree  # noqa: E402

UI = os.environ.get("FLEET_UI_URL", "http://127.0.0.1:3030").rstrip("/")
API = os.environ.get("FLEET_API_URL", "http://127.0.0.1:8080").rstrip("/")
REPO = Path(__file__).resolve().parents[2]
BROWSERS = os.environ.get(
    "PLAYWRIGHT_BROWSERS_PATH",
    str(REPO / ".tmp" / "ms-playwright"),
)
os.environ.setdefault("PLAYWRIGHT_BROWSERS_PATH", BROWSERS)


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


class CatalogRenameTests(unittest.TestCase):
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

    def test_rename_card_enter_saves_escape_cancels(self) -> None:
        page = self.page
        ws, workspace_url = signup_login(page, prefix="rename")
        seed = f"rename-seed-{uuid.uuid4().hex[:8]}"
        created = create_host(page, ws, seed)
        host_id = str(created.get("host", created).get("id") or created.get("id") or "")
        goto_self_hosted(page, workspace_url)
        wait_dx_ready(page)

        assert_no_control_plane_card(page)

        card = open_host_panel(page, seed, timeout_ms=10_000)
        rename_btn = card.get_by_title("Rename")
        self.assertTrue(rename_btn.is_visible(), "side panel pencil must be the Rename control")
        rename_btn.click()

        # Name text lives on the button; after click it is an input value, so
        # do not keep filtering the card with has_text=seed (or the other
        # card inputs: Auto-update checkbox, Max jobs).
        inp = page.locator("article").filter(has_text="pending").locator("input[autofocus]")
        if inp.count() == 0:
            inp = page.locator("input[autofocus]")
        inp.wait_for(timeout=5_000)
        inp.click()
        saved = f"rename-saved-{uuid.uuid4().hex[:8]}"
        inp.fill("")
        inp.press_sequentially(saved, delay=20)
        self.assertEqual(inp.input_value().strip(), saved)
        patch_url = "/worker-hosts"
        if host_id:
            patch_url = f"/worker-hosts/{host_id}"
        try:
            with page.expect_response(
                lambda r: patch_url in r.url
                and r.request.method == "PATCH"
                and r.ok,
                timeout=15_000,
            ) as pending:
                inp.press("Enter")
            pending.value
        except Exception as e:  # noqa: BLE001 — include page dump
            body = page.inner_text("body")
            self.fail(
                f"Enter did not PATCH name={saved!r} ({e}). "
                f"url={page.url} body={body[:1500]!r}"
            )

        try:
            page.get_by_text(saved, exact=True).first.wait_for(timeout=15_000)
        except Exception as e:  # noqa: BLE001
            body = page.inner_text("body")
            self.fail(
                f"saved name not visible after PATCH ({e}). "
                f"url={page.url} body={body[:1500]!r}"
            )
        goto_self_hosted(page, workspace_url)
        wait_dx_ready(page)
        open_host_panel(page, saved, timeout_ms=15_000)
        page.get_by_text(saved, exact=True).first.wait_for(timeout=15_000)
        body = page.inner_text("body")
        self.assertIn(saved, body)
        self.assertNotIn(seed, body)

        page.get_by_title("Rename").click()
        inp = page.locator("input[autofocus]")
        inp.wait_for(timeout=5_000)
        inp.click()
        cancelled = f"rename-esc-{uuid.uuid4().hex[:8]}"
        inp.fill("")
        inp.press_sequentially(cancelled, delay=20)
        inp.press("Escape")

        page.get_by_text(saved, exact=True).first.wait_for(timeout=5_000)
        self.assertNotIn(
            cancelled,
            page.inner_text("body"),
            "Escape must restore the previous name and exit the input",
        )

        goto_self_hosted(page, workspace_url)
        wait_dx_ready(page)
        open_host_panel(page, saved, timeout_ms=15_000)
        page.get_by_text(saved, exact=True).first.wait_for(timeout=15_000)
        body = page.inner_text("body")
        self.assertIn(saved, body)
        self.assertNotIn(cancelled, body)
        assert_no_control_plane_card(page)


if __name__ == "__main__":
    unittest.main(verbosity=2)
