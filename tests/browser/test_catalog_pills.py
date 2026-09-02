#!/usr/bin/env python3
"""Layer 2 e2e: extra pills (catalog:e2e-pending-apply-outdated-pills).

After Apply, pending apply until ack. Behind package (auto-update off)
shows outdated. Both pills can sit on the card together. Heartbeats
update the card via the ~10s list poll — do not navigate after apply
(dx rebuilds stall page.goto). Fail closed — no skip when staged dist
is missing.
"""
from __future__ import annotations

import os
import socket
import sys
import unittest
import uuid
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runners_e2e_helpers import (  # noqa: E402
    API,
    UI,
    goto_self_hosted,
    heartbeat,
    list_hosts,
    pair_and_heartbeat,
    signup_login,
    wait_dx_ready,
)
from stack_identity import ForeignStackError, require_this_tree  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
BROWSERS = os.environ.get(
    "PLAYWRIGHT_BROWSERS_PATH",
    str(REPO / ".tmp" / "ms-playwright"),
)
os.environ.setdefault("PLAYWRIGHT_BROWSERS_PATH", BROWSERS)

OLD_BAKE_ID = "deadbeefdeadbeef"
OLD_BIN_SHA = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
# List poll is 10s; slack for dx overlay + one missed tick.
POLL_WAIT_MS = 20_000


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


def _host_row(page, workspace_id: str, host_id: str) -> dict[str, Any]:
    listed = list_hosts(page, workspace_id)
    for host in listed.get("hosts") or []:
        if host.get("id") == host_id:
            return host
    raise AssertionError(f"host {host_id} missing from list: {listed!r}"[:800])


def _card(page, name: str):
    card = page.locator("article").filter(has_text=name)
    card.first.wait_for(timeout=15_000)
    return card.first


def _staged_linux_manifest(page) -> dict[str, Any] | None:
    res = page.request.get(f"{API}/install/worker/bin/linux-x86_64/manifest.json")
    if not res.ok:
        return None
    try:
        body = res.json()
    except Exception:
        return None
    return body if isinstance(body, dict) else None


def _behind_software(manifest: dict[str, Any] | None) -> dict[str, Any]:
    bake = OLD_BAKE_ID
    host_sha = OLD_BIN_SHA
    if manifest:
        staged = (manifest.get("git_sha") or "").strip()
        if staged and staged.lower() == bake.lower():
            bake = "cafebabecafebabe"
        bins = manifest.get("bins") or {}
        staged_host = bins.get("fleet-host")
        if isinstance(staged_host, dict):
            staged_host = staged_host.get("sha256")
        if isinstance(staged_host, str) and staged_host.strip().lower() == host_sha.lower():
            host_sha = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
    return {
        "git_sha": bake,
        "bins": {
            "fleet-host": host_sha,
            "fleet-driver": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        },
    }


class CatalogPillsTests(unittest.TestCase):
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

    def test_pending_apply_until_ack_and_outdated(self) -> None:
        # catalog:e2e-pending-apply-outdated-pills
        page = self.page
        ws, workspace_url = signup_login(page, prefix="catalog-pills")
        name = f"pills-box-{uuid.uuid4().hex[:12]}"
        created, paired, _hb = pair_and_heartbeat(page, ws, name)
        host_id = created["host"]["id"]
        machine_token = paired["machine_token"]

        row = _host_row(page, ws, host_id)
        self.assertEqual(row.get("status"), "online", row)
        self.assertFalse(row.get("settings_pending"), row)
        self.assertEqual(row.get("settings_gen"), row.get("settings_ack_gen"), row)

        goto_self_hosted(page, workspace_url)
        wait_dx_ready(page)
        page.get_by_role("button", name="Add worker").wait_for(timeout=90_000)
        card = _card(page, name)
        self.assertIn("online", card.inner_text().lower())
        self.assertEqual(
            card.get_by_text("pending apply", exact=True).count(),
            0,
            "fresh pair must not show pending apply",
        )
        self.assertEqual(
            card.get_by_text("outdated", exact=True).count(),
            0,
            "fresh pair must not show outdated",
        )

        # Auto-update off so a behind package is "outdated", not "updating".
        auto = card.locator("label").filter(has_text="Auto-update").locator("input")
        self.assertGreater(auto.count(), 0, "paired card needs Auto-update")
        if auto.first.is_checked():
            auto.first.click(force=True)
        else:
            card.get_by_role("button", name="Stop taking new jobs").click(force=True)
        apply = card.get_by_role("button", name="Apply changes")
        apply.wait_for(timeout=5_000)
        apply.click(force=True)

        pill = card.get_by_text("pending apply", exact=True)
        pill.wait_for(timeout=10_000)
        self.assertTrue(pill.is_visible(), "pending apply must be a card pill, not only a toast")
        self.assertIn("pending apply", card.inner_text())

        row = _host_row(page, ws, host_id)
        self.assertTrue(
            row.get("settings_pending")
            or row.get("settings_gen") != row.get("settings_ack_gen"),
            f"Apply must bump settings_gen: {row}",
        )
        settings_gen = int(row["settings_gen"])
        self.assertGreater(settings_gen, int(row.get("settings_ack_gen") or 0), row)
        self.assertFalse(
            row.get("auto_update", True),
            f"auto-update must be off so outdated (not updating) can show: {row}",
        )

        manifest = _staged_linux_manifest(page)
        self.assertIsNotNone(
            manifest,
            "staged linux-x86_64 manifest is required to assert the outdated "
            "pill (scripts/stage-worker-dist.sh). Missing staged dist is a fail.",
        )
        software = _behind_software(manifest)
        behind = heartbeat(
            page,
            machine_token,
            {
                "platform": "linux-x86_64",
                "hostname": name,
                "software": software,
            },
        )
        behind_host = behind.get("host") or {}
        self.assertTrue(
            bool(behind_host.get("outdated"))
            or behind_host.get("software_status") == "behind",
            f"staged dist must mark non-matching bake behind: {behind_host}",
        )
        self.assertNotEqual(
            behind_host.get("settings_ack_gen"),
            settings_gen,
            f"behind heartbeat must not ack settings: {behind_host}",
        )

        # Stay on the page — next list poll must paint both extra pills.
        card = _card(page, name)
        card.get_by_text("pending apply", exact=True).wait_for(timeout=POLL_WAIT_MS)
        outdated = card.get_by_text("outdated", exact=True)
        try:
            outdated.wait_for(timeout=POLL_WAIT_MS)
        except Exception as e:
            raise AssertionError(
                f"behind + unacked apply must show both pills after the ~10s "
                f"list poll ({e}): {card.inner_text()}"
            ) from e
        self.assertTrue(
            outdated.is_visible(),
            f"behind + unacked apply must show both pills: {card.inner_text()}",
        )
        self.assertIn("pending apply", card.inner_text().lower())

        acked = heartbeat(
            page,
            machine_token,
            {
                "settings_ack_gen": settings_gen,
                "platform": "linux-x86_64",
                "hostname": name,
                "software": software,
            },
        )
        acked_host = acked.get("host") or {}
        self.assertEqual(acked_host.get("settings_ack_gen"), settings_gen, acked)
        self.assertFalse(acked_host.get("settings_pending"), acked)
        self.assertTrue(
            bool(acked_host.get("outdated"))
            or acked_host.get("software_status") == "behind",
            f"ack must not clear outdated: {acked_host}",
        )

        card = _card(page, name)
        pending = card.get_by_text("pending apply", exact=True)
        try:
            pending.wait_for(state="hidden", timeout=POLL_WAIT_MS)
        except Exception as e:
            raise AssertionError(
                f"ack must clear pending apply on the card after the ~10s "
                f"list poll ({e}): {card.inner_text()}"
            ) from e
        self.assertEqual(
            pending.count(),
            0,
            "ack must clear pending apply on the card",
        )
        self.assertNotIn("pending apply", card.inner_text().lower())
        outdated = card.get_by_text("outdated", exact=True)
        try:
            outdated.wait_for(timeout=POLL_WAIT_MS)
        except Exception as e:
            raise AssertionError(
                f"behind package must keep outdated after ack ({e}): "
                f"{card.inner_text()}"
            ) from e
        self.assertTrue(
            outdated.is_visible(),
            f"behind package must keep outdated after ack: {card.inner_text()}",
        )


if __name__ == "__main__":
    unittest.main()
