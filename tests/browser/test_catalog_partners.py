#!/usr/bin/env python3
"""Layer 2 e2e: partners disclaimer ack persists (catalog:e2e-partners-ack-persists).

Ack survives reload and must not enable Lend, Buy, or the partner picker.
"""
from __future__ import annotations

import os
import re
import socket
import sys
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runners_e2e_helpers import UI, signup_login  # noqa: E402
from stack_identity import ForeignStackError, require_this_tree  # noqa: E402

API = os.environ.get("FLEET_API_URL", "http://127.0.0.1:8080").rstrip("/")
REPO = Path(__file__).resolve().parents[2]
BROWSERS = os.environ.get(
    "PLAYWRIGHT_BROWSERS_PATH",
    str(REPO / ".tmp" / "ms-playwright"),
)
os.environ.setdefault("PLAYWRIGHT_BROWSERS_PATH", BROWSERS)

_ACK_CHECKBOX = re.compile(r"acknowledge", re.I)


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


def _goto_partners(page, base: str) -> None:
    """Land on Partners. dx rebuild overlay can hide h1; retry like signup."""
    last_err: Exception | None = None
    for _ in range(24):
        try:
            page.goto(
                f"{base}/runners/partners",
                wait_until="domcontentloaded",
                timeout=15_000,
            )
            page.locator("h1").wait_for(state="visible", timeout=8_000)
            if not re.search(r"/[0-9a-f-]{36}/runners/partners", page.url):
                raise RuntimeError(f"not on partners: {page.url}")
            last_err = None
            break
        except Exception as e:  # noqa: BLE001 — dx mid-rebuild; retry
            last_err = e
            time.sleep(2)
    if last_err is not None:
        raise last_err


class CatalogPartnersAckE2ETests(unittest.TestCase):
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

    def test_ack_persists_and_does_not_enable_lend(self) -> None:
        # catalog:e2e-partners-ack-persists — checkbox POST survives reload; lend stays off.
        from playwright.sync_api import expect

        page = self.page
        _ws, base = signup_login(page, prefix="ack")

        _goto_partners(page, base)
        box = page.get_by_role("checkbox", name=_ACK_CHECKBOX)
        expect(box).not_to_be_checked()

        # Label click: locator.check() retries after the box disables itself.
        label = page.locator("label").filter(has=box)
        with page.expect_response(
            lambda r: "/partners/disclaimer" in r.url
            and r.request.method.upper() == "POST"
            and r.ok,
            timeout=15_000,
        ):
            label.click()

        page.reload(wait_until="domcontentloaded")
        page.locator("h1").wait_for(state="visible", timeout=8_000)
        expect(box).to_be_checked()
        expect(page.get_by_role("button", name="Lend capacity")).to_be_disabled()
        expect(page.get_by_role("button", name="Buy credits")).to_be_disabled()
        expect(page.get_by_role("combobox")).to_be_disabled()


if __name__ == "__main__":
    unittest.main(verbosity=2)
