#!/usr/bin/env python3
"""Layer 2 e2e: software line on paired boxes only (catalog:e2e-software-line).

Software line after pair; not on the plane or a pending card. Bake id is
software.git_sha (12-char short), never a bin file hash.
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
    create_host,
    goto_self_hosted,
    list_hosts,
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

BAKE_SHA = "9d239a509055abcdef"
BAKE_SHORT = "9d239a509055"
BIN_SHA = "2a971d693b6f7f0c13c94269845ca4ac0c73672b72ac7a2c7839be2cee590c8e"
BIN_PREFIX = "2a971d69"
PLATFORM = "linux-x86_64"
SOFTWARE_FLAGS = ("up to date", "outdated", "unknown", "old package", "updating")


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
    try:
        page.get_by_text("Control plane", exact=True).first.wait_for(timeout=30_000)
    except Exception as e:
        raise AssertionError(
            f"Control plane missing: {e}. body={page.inner_text('body')[:800]!r}"
        ) from e


def _article(page, name: str):
    """Named host card; Control plane is never a match."""
    page.get_by_text(name).first.wait_for(timeout=30_000)
    card = page.locator("article").filter(has_text=name)
    card.first.wait_for(timeout=15_000)
    for i in range(card.count()):
        art = card.nth(i)
        text = art.inner_text()
        if "Control plane" in text:
            continue
        if name in text:
            return art
    raise AssertionError(
        f"host card {name!r} missing (or only Control plane matched). "
        f"body={page.inner_text('body')[:800]!r}"
    )


def _software_extra() -> dict:
    return {
        "software": {
            "git_sha": BAKE_SHA,
            "bins": {"fleet-host": BIN_SHA},
            "updating": False,
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

    def test_control_plane_card_has_no_software_line(self) -> None:
        _reload_self_hosted(self.page, self.workspace_url)
        first = self.page.locator("article").first
        first.wait_for(timeout=10_000)
        text = first.inner_text()
        self.assertIn("Control plane", text)
        self.assertNotIn("software ", text.lower())

    def test_paired_card_shows_bake_id_not_bin_hash(self) -> None:
        name = f"sw-paired-{uuid.uuid4().hex[:8]}"
        created, paired, hb = pair_and_heartbeat(
            self.page,
            self.workspace_id,
            name,
            platform=PLATFORM,
            heartbeat_extra=_software_extra(),
        )
        self.assertIn("pair_token", created)
        self.assertIn("machine_token", paired)
        host = hb.get("host") or {}
        reported = host.get("software") or {}
        self.assertEqual(reported.get("git_sha"), BAKE_SHA, hb)
        listed = list_hosts(self.page, self.workspace_id)
        row = next(
            (h for h in (listed.get("hosts") or []) if h.get("name") == name),
            None,
        )
        self.assertIsNotNone(row, listed)
        self.assertEqual((row.get("software") or {}).get("git_sha"), BAKE_SHA)

        _reload_self_hosted(self.page, self.workspace_url)
        plane = self.page.locator("article").first
        plane.wait_for(timeout=10_000)
        plane_text = plane.inner_text()
        self.assertIn("Control plane", plane_text)
        self.assertNotIn("software ", plane_text.lower())

        card = _article(self.page, name)
        self.assertNotIn("Control plane", card.inner_text())
        card.get_by_text(BAKE_SHORT).wait_for(timeout=15_000)
        text = card.inner_text()
        lowered = text.lower()
        self.assertIn("software ", lowered)
        self.assertIn(BAKE_SHORT, text)
        self.assertIn(PLATFORM, text)
        self.assertNotIn(
            BIN_PREFIX,
            text,
            "file hash must not be used as the bake / version id",
        )
        self.assertRegex(
            lowered,
            rf"software\s+{BAKE_SHORT}\s+·\s+{PLATFORM}\s+·\s+"
            rf"({'|'.join(SOFTWARE_FLAGS)})",
        )

    def test_pending_card_has_no_software_line(self) -> None:
        name = f"sw-pending-{uuid.uuid4().hex[:8]}"
        created = create_host(self.page, self.workspace_id, name)
        self.assertIn("pair_token", created)
        _reload_self_hosted(self.page, self.workspace_url)
        card = _article(self.page, name)
        text = card.inner_text()
        self.assertIn("pending", text.lower())
        self.assertNotIn("software ", text.lower())


if __name__ == "__main__":
    unittest.main(verbosity=2)
