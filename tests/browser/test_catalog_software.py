#!/usr/bin/env python3
"""Layer 2 e2e: software bin rows on paired boxes (catalog:e2e-software-line).

Paired HostDetail paints fleet-host / fleet-driver as
'{name} {semver} · {status}'.
"""
from __future__ import annotations

import os
import re
import socket
import sys
import unittest
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runners_e2e_helpers import (  # noqa: E402
    goto_self_hosted,
    pair_and_heartbeat,
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

HOST_VER = "3.1.4-e2ehost"
DRIVER_VER = "5.0.0-e2edriver"
PLATFORM = "linux-x86_64"
_BIN_STATUS = r"(current|behind|missing|reported|unknown)"


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


def _reload_self_hosted(page, workspace_url: str) -> None:
    goto_self_hosted(page, workspace_url)
    wait_dx_ready(page, timeout_ms=45_000)
    page.get_by_role("button", name="Add worker").wait_for(timeout=30_000)


def _software_extra() -> dict:
    return {
        "software": {
            "bins": {
                "fleet-host": {"version": HOST_VER},
                "fleet-driver": {"version": DRIVER_VER},
            }
        }
    }


class CatalogSoftwareLineTests(unittest.TestCase):
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
        cls.workspace_id, cls.workspace_url = signup_login(cls.page, prefix="catalog-sw")

    @classmethod
    def tearDownClass(cls) -> None:
        cls._context.close()
        cls._browser.close()
        cls._pw.stop()

    def test_paired_card_shows_bin_name_semver_status(self) -> None:
        name = f"sw-paired-{uuid.uuid4().hex[:8]}"
        created, paired, _hb = pair_and_heartbeat(
            self.page,
            self.workspace_id,
            name,
            platform=PLATFORM,
            heartbeat_extra=_software_extra(),
        )
        self.assertIn("pair_token", created)
        self.assertIn("machine_token", paired)

        _reload_self_hosted(self.page, self.workspace_url)
        # software_bin_rows live on HostDetailPanel, not the list button.
        row = self.page.get_by_role("button").filter(has_text=name)
        row.first.wait_for(timeout=30_000)
        row.first.click()
        card = self.page.locator("article").filter(has_text=name)
        card.first.wait_for(timeout=15_000)
        text = card.first.inner_text()
        self.assertRegex(
            text,
            rf"fleet-host {re.escape(HOST_VER)} · {_BIN_STATUS}",
        )
        self.assertRegex(
            text,
            rf"fleet-driver {re.escape(DRIVER_VER)} · {_BIN_STATUS}",
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
