#!/usr/bin/env python3
"""Layer 2 e2e: widgets lock while pending (catalog:e2e-widgets-lock-pending).

Inputs stay editable until Apply. After Apply they stay disabled (including
across reload) until the host acks settings_gen. Fail closed — no skip.
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
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runners_e2e_helpers import (  # noqa: E402
    API,
    UI,
    goto_self_hosted,
    heartbeat,
    list_hosts,
    pair_and_heartbeat,
    wait_dx_ready,
)
from stack_identity import ForeignStackError, require_this_tree  # noqa: E402

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


def _host_row(page, workspace_id: str, host_id: str) -> dict[str, Any]:
    listed = list_hosts(page, workspace_id)
    for host in listed.get("hosts") or []:
        if host.get("id") == host_id:
            return host
    raise AssertionError(f"host {host_id} missing from list: {listed!r}"[:800])


def _signup_api(page) -> tuple[str, str]:
    """Cookie session via API so a stuck dx rebuild banner cannot hang signup."""
    email = f"catalog-wlock-{uuid.uuid4().hex[:12]}@example.test"
    password = "e2e-pass-ok"
    headers = {"Content-Type": "application/json"}
    created = page.request.post(
        f"{UI}/api/auth/signup",
        data=json.dumps(
            {
                "email": email,
                "password": password,
                "display_name": "E2E catalog-wlock",
            }
        ),
        headers=headers,
    )
    if created.status not in {200, 201}:
        raise RuntimeError(f"signup {created.status}: {created.text()[:400]}")
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
        spaces = (me.json() or {}).get("workspaces") or []
        ws = str((spaces[0].get("id") if spaces else "") or "")
    if not ws:
        raise RuntimeError(f"login missing workspace: {body!r}"[:400])
    return ws, f"{UI}/{ws}"


def _card(page, name: str):
    card = page.locator("article").filter(has_text=name)
    card.first.wait_for(timeout=15_000)
    found = card.first
    text = found.inner_text()
    if "Control plane" in text:
        raise AssertionError(f"matched Control plane instead of {name!r}: {text[:400]!r}")
    return found


def _widgets(card):
    auto = card.locator("label").filter(has_text="Auto-update").locator("input")
    max_jobs = card.locator("label").filter(has_text="Max jobs at once").locator("input")
    native = card.get_by_role("checkbox", name=re.compile(r"^Native$"))
    if native.count() == 0:
        native = card.locator("label").filter(has_text=re.compile(r"^Native$")).locator(
            "input"
        )
    sandboxed = card.get_by_role("checkbox", name=re.compile(r"Sandboxed"))
    if sandboxed.count() == 0:
        sandboxed = card.locator("label").filter(has_text="Sandboxed").locator("input")
    return auto, max_jobs, native, sandboxed


class CatalogWidgetsLockPendingTests(unittest.TestCase):
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

    def test_widgets_lock_while_pending_until_ack(self) -> None:
        # catalog:e2e-widgets-lock-pending — Inputs disabled while pending.
        page = self.page
        ws, workspace_url = _signup_api(page)
        name = f"wlock-{uuid.uuid4().hex[:12]}"
        created, paired, _hb = pair_and_heartbeat(page, ws, name)
        host = created.get("host") if isinstance(created.get("host"), dict) else created
        host_id = host.get("id")
        self.assertTrue(host_id, created)
        machine_token = paired["machine_token"]

        row = _host_row(page, ws, host_id)
        self.assertEqual(row.get("status"), "online", row)
        self.assertFalse(row.get("settings_pending"), row)
        self.assertEqual(row.get("settings_gen"), row.get("settings_ack_gen"), row)

        goto_self_hosted(page, workspace_url)
        wait_dx_ready(page)
        card = _card(page, name)
        self.assertIn("online", card.inner_text().lower())
        auto, max_jobs, native, sandboxed = _widgets(card)
        self.assertGreater(auto.count(), 0, "paired card needs Auto-update")
        self.assertGreater(max_jobs.count(), 0, "paired card needs Max jobs at once")
        self.assertGreater(native.count(), 0, "paired card needs Native")
        self.assertGreater(sandboxed.count(), 0, "paired card needs Sandboxed")
        self.assertTrue(auto.first.is_enabled(), "Auto-update must be editable before Apply")
        self.assertTrue(max_jobs.first.is_enabled(), "max jobs must be editable before Apply")
        self.assertTrue(native.first.is_enabled(), "Native must be editable before Apply")
        self.assertTrue(sandboxed.first.is_enabled(), "Sandboxed must be editable before Apply")
        self.assertEqual(
            card.get_by_text("pending apply", exact=True).count(),
            0,
            "fresh pair must not show pending apply",
        )

        auto.first.click(force=True)
        apply = card.get_by_role("button", name="Apply changes")
        apply.wait_for(timeout=5_000)
        apply.click(force=True)

        # List poll is 10s; do not wait on the in-memory card. Server gen is the lock.
        row = {}
        settings_gen = 0
        for _ in range(30):
            row = _host_row(page, ws, host_id)
            if row.get("settings_pending") or row.get("settings_gen") != row.get(
                "settings_ack_gen"
            ):
                settings_gen = int(row["settings_gen"])
                break
            page.wait_for_timeout(400)
        self.assertTrue(
            settings_gen
            and (
                row.get("settings_pending")
                or row.get("settings_gen") != row.get("settings_ack_gen")
            ),
            f"Apply must bump settings_gen: {row}",
        )
        self.assertGreater(settings_gen, int(row.get("settings_ack_gen") or 0), row)

        # Reload: lock must come from server pending, not the in-flight busy flag.
        goto_self_hosted(page, workspace_url)
        wait_dx_ready(page)
        card = _card(page, name)
        try:
            card.get_by_text("pending apply", exact=True).wait_for(timeout=20_000)
        except Exception:
            raise AssertionError(
                "pending apply pill missing after reload: "
                f"{card.inner_text()[:800]!r} api={row!r}"
            ) from None
        auto, max_jobs, native, sandboxed = _widgets(card)
        self.assertFalse(auto.first.is_enabled(), "Auto-update must lock while pending apply")
        self.assertFalse(
            max_jobs.first.is_enabled(),
            "Max jobs at once must lock while pending apply",
        )
        self.assertFalse(native.first.is_enabled(), "Isolation Native must lock while pending apply")
        self.assertFalse(
            sandboxed.first.is_enabled(),
            "Isolation Sandboxed must lock while pending apply",
        )

        acked = heartbeat(page, machine_token, {"settings_ack_gen": settings_gen})
        acked_host = acked.get("host") or {}
        self.assertEqual(acked_host.get("settings_ack_gen"), settings_gen, acked)
        self.assertFalse(acked_host.get("settings_pending"), acked)

        goto_self_hosted(page, workspace_url)
        wait_dx_ready(page)
        card = _card(page, name)
        self.assertEqual(
            card.get_by_text("pending apply", exact=True).count(),
            0,
            "ack must clear pending apply so widgets unlock",
        )
        auto, max_jobs, native, sandboxed = _widgets(card)
        self.assertTrue(auto.first.is_enabled(), "Auto-update must unlock after ack")
        self.assertTrue(max_jobs.first.is_enabled(), "max jobs must unlock after ack")
        self.assertTrue(native.first.is_enabled(), "Native must unlock after ack")
        self.assertTrue(sandboxed.first.is_enabled(), "Sandboxed must unlock after ack")


if __name__ == "__main__":
    unittest.main(verbosity=2)
