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
    list_hosts,
    pair_and_heartbeat,
)
from stack_identity import ForeignStackError, require_this_tree  # noqa: E402

API = os.environ.get("FLEET_API_URL", "http://127.0.0.1:8080").rstrip("/")
REPO = Path(__file__).resolve().parents[2]
BROWSERS = os.environ.get(
    "PLAYWRIGHT_BROWSERS_PATH",
    str(REPO / ".tmp" / "ms-playwright"),
)
os.environ.setdefault("PLAYWRIGHT_BROWSERS_PATH", BROWSERS)

SHOW_REMOVED = re.compile(r"^Show removed \([1-9]\d*\)$")
HIDE_REMOVED = re.compile(r"^Hide removed \([1-9]\d*\)$")
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


def _list_row(page, name: str):
    """HostListRow button (list row, not the detail article)."""
    return (
        page.get_by_role("button")
        .filter(has_text=name)
        .filter(has_text=re.compile(r"last seen", re.I))
    )


def _listed_host(listed: dict, host_id: str) -> dict:
    for host in listed.get("hosts") or []:
        if host.get("id") == host_id:
            return host
    raise AssertionError(f"host {host_id} missing from list: {listed!r}"[:800])


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
        # catalog:e2e-show-removed — Confirm paired revoke, then Show/Hide removed.
        page = self.page
        ws, workspace_url = _api_workspace(page)
        leftover = f"never-paired-{uuid.uuid4().hex[:8]}"
        paired_name = f"paired-rm-{uuid.uuid4().hex[:8]}"

        leftover_created = create_host(page, ws, leftover)
        leftover_id = str(leftover_created["host"]["id"])
        revoked = page.request.delete(
            f"{UI}/api/workspaces/{ws}/worker-hosts/{leftover_id}"
        )
        self.assertTrue(
            revoked.ok,
            f"revoke leftover {leftover_id}: {revoked.status} {revoked.text()[:300]}",
        )

        created, _paired, _hb = pair_and_heartbeat(page, ws, paired_name)
        host = created.get("host") if isinstance(created.get("host"), dict) else created
        paired_id = str(host.get("id") or "")
        self.assertTrue(paired_id, created)

        goto_self_hosted(page, workspace_url)
        row = _list_row(page, paired_name)
        try:
            row.first.wait_for(timeout=30_000)
        except Exception as e:
            raise AssertionError(
                f"paired list row {paired_name!r} missing. "
                f"body={page.inner_text('body')[:800]!r}"
            ) from e
        row.first.click()

        page.get_by_role("button", name="Remove worker").wait_for(timeout=10_000)
        page.get_by_role("button", name="Remove worker").click()
        page.get_by_text(CONFIRM_COPY).wait_for(timeout=5_000)
        page.get_by_role("button", name="Confirm").click()
        row.wait_for(state="detached", timeout=15_000)

        listed = list_hosts(page, ws)
        paired_row = _listed_host(listed, paired_id)
        self.assertEqual(
            paired_row.get("status"),
            "disabled",
            f"Confirm must persist disabled on GET list: {paired_row}",
        )
        leftover_row = _listed_host(listed, leftover_id)
        self.assertEqual(
            leftover_row.get("status"),
            "disabled",
            f"never-paired leftover must stay on GET list as disabled: {leftover_row}",
        )

        show = page.get_by_role("button", name=SHOW_REMOVED)
        show.wait_for(timeout=5_000)
        show.click()

        page.get_by_role("button", name=HIDE_REMOVED).wait_for(timeout=5_000)
        restored = _list_row(page, paired_name)
        try:
            restored.first.wait_for(timeout=10_000)
        except Exception as e:
            raise AssertionError(
                f"Show removed must reveal {paired_name!r}. "
                f"body={page.inner_text('body')[:800]!r}"
            ) from e
        self.assertRegex(restored.first.inner_text().lower(), r"\bremoved\b")
        self.assertEqual(
            page.get_by_text(leftover, exact=True).count(),
            0,
            "never-paired leftover must stay hidden after Show removed",
        )

        page.get_by_role("button", name=HIDE_REMOVED).click()
        page.get_by_role("button", name=SHOW_REMOVED).wait_for(timeout=5_000)
        restored.wait_for(state="detached", timeout=10_000)


if __name__ == "__main__":
    unittest.main(verbosity=2)
