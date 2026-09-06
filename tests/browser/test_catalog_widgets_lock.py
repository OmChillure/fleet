#!/usr/bin/env python3
"""Layer 2 e2e: widgets lock while pending (catalog:e2e-widgets-lock-pending).

Inputs stay editable until Apply. After Apply they stay disabled (including
across reload) until the host acks settings_gen. Fail closed — no skip.
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
    return auto, max_jobs


def _sandbox_widgets(card):
    allow = card.locator("label").filter(has_text="Allow process jobs").locator("input")
    work_root = card.locator("label").filter(has_text="Work root").locator("select")
    return allow, work_root


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
        from playwright.sync_api import expect

        page = self.page
        ws, workspace_url = _signup_api(page)
        name = f"wlock-{uuid.uuid4().hex[:12]}"
        created, paired, _hb = pair_and_heartbeat(page, ws, name)
        host = created.get("host") if isinstance(created.get("host"), dict) else created
        host_id = host.get("id")
        self.assertTrue(host_id, created)
        machine_token = paired["machine_token"]

        goto_self_hosted(page, workspace_url)
        wait_dx_ready(page)
        card = _card(page, name)
        auto, max_jobs = _widgets(card)
        expect(auto.first, "Auto-update must be editable before Apply").to_be_enabled()
        expect(max_jobs.first, "max jobs must be editable before Apply").to_be_enabled()

        auto.first.click(force=True)
        apply = card.get_by_role("button", name="Apply changes")
        apply.wait_for(timeout=5_000)
        apply.click(force=True)
        # Stay until PATCH lands so reload does not abort it.
        apply.wait_for(state="hidden", timeout=20_000)

        # Reload: lock must come from server pending, not the in-flight busy flag.
        goto_self_hosted(page, workspace_url)
        wait_dx_ready(page)
        card = _card(page, name)
        expect(
            card.get_by_text("pending apply", exact=True),
            "pending apply pill missing after reload",
        ).to_be_visible(timeout=20_000)
        auto, max_jobs = _widgets(card)
        expect(auto.first, "Auto-update must lock while pending apply").to_be_disabled()
        expect(max_jobs.first, "Max jobs at once must lock while pending apply").to_be_disabled()

        row = _host_row(page, ws, host_id)
        settings_gen = int(row.get("settings_gen") or 0)
        self.assertGreater(
            settings_gen,
            int(row.get("settings_ack_gen") or 0),
            f"Apply must bump settings_gen: {row}",
        )

        heartbeat(page, machine_token, {"settings_ack_gen": settings_gen})

        goto_self_hosted(page, workspace_url)
        wait_dx_ready(page)
        card = _card(page, name)
        auto, max_jobs = _widgets(card)
        expect(auto.first, "Auto-update must unlock after ack").to_be_enabled()
        expect(max_jobs.first, "max jobs must unlock after ack").to_be_enabled()

    def test_sandbox_allow_process_and_work_root_lock_while_pending(self) -> None:
        # SandboxBlock pending lock — Allow process jobs / Work root.
        from playwright.sync_api import expect

        page = self.page
        ws, workspace_url = _signup_api(page)
        name = f"wlock-sbx-{uuid.uuid4().hex[:12]}"
        created, paired, _hb = pair_and_heartbeat(page, ws, name)
        host = created.get("host") if isinstance(created.get("host"), dict) else created
        host_id = host.get("id")
        self.assertTrue(host_id, created)
        machine_token = paired["machine_token"]

        goto_self_hosted(page, workspace_url)
        wait_dx_ready(page)
        card = _card(page, name)
        allow, work_root = _sandbox_widgets(card)
        expect(allow.first, "Allow process jobs must be editable before Apply").to_be_enabled()
        expect(work_root.first, "Work root must be editable before Apply").to_be_enabled()

        auto, _max_jobs = _widgets(card)
        auto.first.click(force=True)
        apply = card.get_by_role("button", name="Apply changes")
        apply.wait_for(timeout=5_000)
        apply.click(force=True)
        apply.wait_for(state="hidden", timeout=20_000)

        goto_self_hosted(page, workspace_url)
        wait_dx_ready(page)
        card = _card(page, name)
        expect(
            card.get_by_text("pending apply", exact=True),
            "pending apply pill missing after reload",
        ).to_be_visible(timeout=20_000)
        allow, work_root = _sandbox_widgets(card)
        expect(allow.first, "Allow process jobs must lock while pending apply").to_be_disabled()
        expect(work_root.first, "Work root must lock while pending apply").to_be_disabled()

        row = _host_row(page, ws, host_id)
        settings_gen = int(row.get("settings_gen") or 0)
        self.assertGreater(
            settings_gen,
            int(row.get("settings_ack_gen") or 0),
            f"Apply must bump settings_gen: {row}",
        )

        heartbeat(page, machine_token, {"settings_ack_gen": settings_gen})

        goto_self_hosted(page, workspace_url)
        wait_dx_ready(page)
        card = _card(page, name)
        allow, work_root = _sandbox_widgets(card)
        expect(allow.first, "Allow process jobs must unlock after ack").to_be_enabled()
        expect(work_root.first, "Work root must unlock after ack").to_be_enabled()


if __name__ == "__main__":
    unittest.main(verbosity=2)
