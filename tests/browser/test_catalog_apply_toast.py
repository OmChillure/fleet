#!/usr/bin/env python3
"""Layer 2 e2e: Apply PATCH toast + pending apply (catalog:e2e-apply-toast-pending).

Toggle a paired card → Apply appears → PATCH bumps settings_gen. Toast is
the pending-ack copy ("Settings sent. Waiting for the runner."), not the
plane's immediate "Settings applied." The card shows pending apply until
the host acks. Fail closed — no skip.
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

TOAST = "Settings sent. Waiting for the runner."
PLANE_TOAST = "Settings applied."
# List poll is 10s; slack for dx overlay + one missed tick.
POLL_WAIT_MS = 20_000


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
    email = f"catalog-apply-toast-{uuid.uuid4().hex[:12]}@example.test"
    password = "e2e-pass-ok"
    headers = {"Content-Type": "application/json"}
    created = page.request.post(
        f"{UI}/api/auth/signup",
        data=json.dumps(
            {
                "email": email,
                "password": password,
                "display_name": "E2E catalog-apply-toast",
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


class CatalogApplyToastPendingTests(unittest.TestCase):
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

    def test_apply_toast_and_pending_apply_until_ack(self) -> None:
        # catalog:e2e-apply-toast-pending — Apply PATCH toast + pending apply.
        page = self.page
        ws, workspace_url = _signup_api(page)
        name = f"apply-toast-{uuid.uuid4().hex[:12]}"
        created, paired, _hb = pair_and_heartbeat(page, ws, name)
        host = created.get("host") if isinstance(created.get("host"), dict) else created
        host_id = host.get("id")
        machine_token = paired["machine_token"]
        self.assertTrue(host_id, created)
        self.assertTrue(machine_token, paired)

        row = _host_row(page, ws, host_id)
        self.assertEqual(row.get("status"), "online", row)
        self.assertFalse(row.get("settings_pending"), row)
        self.assertEqual(row.get("settings_gen"), row.get("settings_ack_gen"), row)
        gen_before = int(row.get("settings_gen") or 0)

        goto_self_hosted(page, workspace_url)
        page.get_by_role("button", name="Add worker").wait_for(timeout=90_000)
        wait_dx_ready(page)
        card = _card(page, name)
        self.assertIn("online", card.inner_text().lower())
        self.assertEqual(
            card.get_by_text("pending apply", exact=True).count(),
            0,
            "fresh pair must not show pending apply",
        )

        auto = card.locator("label").filter(has_text="Auto-update").locator("input")
        self.assertGreater(auto.count(), 0, "paired card needs Auto-update")
        apply = card.get_by_role("button", name="Apply changes")
        self.assertEqual(
            apply.count(),
            0,
            "Apply must stay hidden until Auto-update is toggled",
        )
        auto.first.click(force=True)
        apply.wait_for(state="attached", timeout=10_000)

        # Toast lasts ~4s — do not wait_dx_ready between click and the status.
        toast = page.get_by_role("status").filter(has_text=TOAST)
        apply.click(force=True)
        try:
            toast.first.wait_for(state="visible", timeout=15_000)
        except Exception as e:
            statuses = page.get_by_role("status")
            seen = [
                statuses.nth(i).inner_text()
                for i in range(statuses.count())
            ]
            raise AssertionError(
                f"Apply toast missing ({e}). statuses={seen!r} "
                f"card={card.inner_text()[:600]!r} body={page.inner_text('body')[:800]!r}"
            ) from e
        toast_text = toast.first.inner_text()
        self.assertIn(
            TOAST,
            toast_text,
            "Apply toast must use the pending-ack copy, not plane 'Settings applied.'",
        )
        self.assertNotIn(
            PLANE_TOAST,
            toast_text,
            "paired Apply must not use the plane's immediate 'Settings applied.' toast",
        )

        pill = card.get_by_text("pending apply", exact=True)
        try:
            pill.wait_for(timeout=POLL_WAIT_MS)
        except Exception as e:
            raise AssertionError(
                f"Apply must show pending apply on the card, not only a toast "
                f"({e}): {card.inner_text()[:800]!r}"
            ) from e
        self.assertTrue(
            pill.is_visible(),
            "Apply must show pending apply on the card, not only a toast",
        )
        self.assertIn("pending apply", card.inner_text())

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
        self.assertGreater(
            settings_gen,
            int(row.get("settings_ack_gen") or 0),
            f"Apply must bump settings_gen: {row}",
        )
        self.assertGreater(
            settings_gen,
            gen_before,
            f"Apply PATCH must increase settings_gen from {gen_before}: {row}",
        )

        acked = heartbeat(page, machine_token, {"settings_ack_gen": settings_gen})
        acked_host = acked.get("host") or {}
        self.assertEqual(acked_host.get("settings_ack_gen"), settings_gen, acked)
        self.assertFalse(acked_host.get("settings_pending"), acked)

        # Stay on the page — next list poll must drop the pill (goto can stall on dx).
        card = _card(page, name)
        pending = card.get_by_text("pending apply", exact=True)
        try:
            pending.wait_for(state="hidden", timeout=POLL_WAIT_MS)
        except Exception as e:
            raise AssertionError(
                f"ack must clear pending apply on the card after the ~10s "
                f"list poll ({e}): {card.inner_text()[:800]!r}"
            ) from e
        self.assertEqual(
            pending.count(),
            0,
            "ack must clear pending apply on the card",
        )
        self.assertNotIn("pending apply", card.inner_text().lower())


if __name__ == "__main__":
    unittest.main(verbosity=2)
