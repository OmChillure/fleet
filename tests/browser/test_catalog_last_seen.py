#!/usr/bin/env python3
"""Layer 2 e2e: last seen relative (catalog:e2e-last-seen).

Pending list row: last seen never. After pair+heartbeat: just now / relative.
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
    create_host,
    goto_self_hosted,
    heartbeat,
    list_hosts,
    pair_host,
    signup_login,
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

# relative_seen: never | just now | Ns/m/h/d ago
LIVE_SEEN = r"last seen (just now|\d+[smhd] ago)"


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


def _list_button_for_host(page, name: str):
    """Wait for the named HostListRow button (list row, not detail article)."""
    row = (
        page.get_by_role("button")
        .filter(has_text=name)
        .filter(has_text=re.compile(r"last seen", re.I))
    )
    try:
        row.first.wait_for(timeout=30_000)
    except Exception as e:
        raise AssertionError(
            f"host list button {name!r} missing. "
            f"body={page.inner_text('body')[:800]!r}"
        ) from e
    return row.first


def _listed_host(page, workspace_id: str, host_id: str) -> dict:
    listed = list_hosts(page, workspace_id)
    for host in listed.get("hosts") or []:
        if host.get("id") == host_id:
            return host
    raise AssertionError(f"host {host_id} missing from list: {listed!r}"[:800])


class CatalogLastSeenE2ETests(unittest.TestCase):
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

    def test_last_seen_never_then_just_now(self) -> None:
        page = self.page
        ws, workspace_url = signup_login(page, prefix="last-seen")
        name = f"seen-{uuid.uuid4().hex[:8]}"
        created = create_host(page, ws, name)
        token = created["pair_token"]
        host = created.get("host") if isinstance(created.get("host"), dict) else created
        host_id = host.get("id")
        self.assertTrue(host_id, created)

        pending_api = _listed_host(page, ws, host_id)
        self.assertIsNone(
            pending_api.get("last_seen_at"),
            f"create must leave last_seen_at null: {pending_api!r}"[:800],
        )

        goto_self_hosted(page, workspace_url)
        page.get_by_role("button", name="Add worker").wait_for(timeout=45_000)

        pending = _list_button_for_host(page, name)
        pending.get_by_text("last seen never", exact=False).wait_for(timeout=15_000)
        pending_text = pending.inner_text()
        self.assertIn(
            "last seen never",
            pending_text.lower(),
            "pending/unpaired list row must show last seen never",
        )

        paired = pair_host(
            page,
            token,
            hostname=name,
            platform="linux-x86_64",
        )
        heartbeat(
            page,
            paired["machine_token"],
            {"hostname": name, "platform": "linux-x86_64"},
        )

        live_api = _listed_host(page, ws, host_id)
        seen_at = live_api.get("last_seen_at")
        self.assertTrue(
            isinstance(seen_at, str) and seen_at.strip(),
            f"pair+heartbeat must set last_seen_at: {live_api!r}"[:800],
        )

        goto_self_hosted(page, workspace_url)
        page.get_by_text(name, exact=True).first.wait_for(timeout=30_000)
        live = _list_button_for_host(page, name)
        live.get_by_text(re.compile(LIVE_SEEN, re.I)).wait_for(timeout=15_000)
        live_text = live.inner_text()
        self.assertNotIn(
            "last seen never",
            live_text.lower(),
            "paired+heartbeat list row must leave never",
        )
        self.assertRegex(
            live_text.lower(),
            LIVE_SEEN,
            "live list row must show just now or relative last seen after pair+heartbeat",
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
