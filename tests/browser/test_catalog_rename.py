#!/usr/bin/env python3
"""Layer 2 e2e: rename a Self-hosted card (Enter saves, Escape cancels).

Catalog: List, identity, status → Rename (click name, Enter / Escape).
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
    list_hosts,
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


def _list_row(page, name: str):
    """HostListRow button (list row, not the detail title=Rename control)."""
    return (
        page.get_by_role("button")
        .filter(has_text=name)
        .filter(has_text=re.compile(r"last seen", re.I))
    )


def _listed_host(page, workspace_id: str, host_id: str) -> dict:
    listed = list_hosts(page, workspace_id)
    for host in listed.get("hosts") or []:
        if str(host.get("id")) == str(host_id):
            return host
    raise AssertionError(f"host {host_id} missing from list: {listed!r}"[:800])


def _is_host_patch(req, host_id: str) -> bool:
    return (
        req.method == "PATCH"
        and "/worker-hosts/" in req.url
        and str(host_id) in req.url
    )


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
        from playwright.sync_api import expect

        page = self.page
        ws, workspace_url = signup_login(page, prefix="rename")
        seed = f"rename-seed-{uuid.uuid4().hex[:8]}"
        created = create_host(page, ws, seed)
        host = created.get("host") if isinstance(created.get("host"), dict) else created
        host_id = host.get("id")
        self.assertTrue(host_id, created)
        goto_self_hosted(page, workspace_url)

        row = _list_row(page, seed)
        expect(row).to_be_visible()
        row.click()
        rename = page.get_by_title("Rename")
        expect(rename).to_have_text(seed)
        rename.click()
        # Rename editor has no accessible name; autofocus is the public marker.
        inp = page.locator("input[autofocus]")
        expect(inp).to_be_visible()
        saved = f"rename-saved-{uuid.uuid4().hex[:8]}"
        inp.fill(saved)
        try:
            with page.expect_response(
                lambda r: _is_host_patch(r.request, host_id) and r.ok,
                timeout=15_000,
            ) as pending:
                inp.press("Enter")
            pending.value
        except Exception as e:  # noqa: BLE001 — include page dump
            self.fail(
                f"Enter did not PATCH name={saved!r} ({e}). "
                f"url={page.url} body={page.inner_text('body')[:1500]!r}"
            )
        expect(page.get_by_title("Rename")).to_have_text(saved)
        listed = _listed_host(page, ws, host_id)
        self.assertEqual(
            listed.get("name"),
            saved,
            f"Enter must persist name via list_hosts: {listed!r}"[:800],
        )

        page.get_by_title("Rename").click()
        inp = page.locator("input[autofocus]")
        expect(inp).to_be_visible()
        cancelled = f"rename-esc-{uuid.uuid4().hex[:8]}"
        inp.fill(cancelled)
        patches: list[str] = []

        def _on_req(req) -> None:
            if _is_host_patch(req, host_id):
                patches.append(req.url)

        page.on("request", _on_req)
        try:
            inp.press("Escape")
            expect(page.get_by_title("Rename")).to_have_text(saved)
        finally:
            page.remove_listener("request", _on_req)
        self.assertEqual(patches, [], f"Escape must not PATCH: {patches!r}")
        listed = _listed_host(page, ws, host_id)
        self.assertEqual(
            listed.get("name"),
            saved,
            f"Escape must leave the saved name: {listed!r}"[:800],
        )

    def test_rename_enter_empty_or_unchanged_closes_without_patch(self) -> None:
        from playwright.sync_api import expect

        page = self.page
        ws, workspace_url = signup_login(page, prefix="rename-empty")
        seed = f"rename-empty-{uuid.uuid4().hex[:8]}"
        created = create_host(page, ws, seed)
        host = created.get("host") if isinstance(created.get("host"), dict) else created
        host_id = host.get("id")
        self.assertTrue(host_id, created)
        goto_self_hosted(page, workspace_url)

        row = _list_row(page, seed)
        expect(row).to_be_visible()
        row.click()
        rename = page.get_by_title("Rename")
        expect(rename).to_have_text(seed)
        rename.click()
        inp = page.locator("input[autofocus]")
        expect(inp).to_be_visible()
        patches: list[str] = []

        def _on_req(req) -> None:
            if _is_host_patch(req, host_id):
                patches.append(req.url)

        page.on("request", _on_req)
        try:
            inp.fill("")
            inp.press("Enter")
            expect(page.get_by_title("Rename")).to_have_text(seed)

            page.get_by_title("Rename").click()
            inp = page.locator("input[autofocus]")
            expect(inp).to_be_visible()
            expect(inp).to_have_value(seed)
            inp.press("Enter")
            expect(page.get_by_title("Rename")).to_have_text(seed)
        finally:
            page.remove_listener("request", _on_req)
        self.assertEqual(
            patches,
            [],
            f"empty/unchanged Enter must not PATCH: {patches!r}",
        )
        listed = _listed_host(page, ws, host_id)
        self.assertEqual(listed.get("name"), seed, listed)

    def test_rename_patch_error_toasts_and_keeps_old_name(self) -> None:
        from playwright.sync_api import expect

        page = self.page
        ws, workspace_url = signup_login(page, prefix="rename-err")
        seed = f"rename-err-{uuid.uuid4().hex[:8]}"
        created = create_host(page, ws, seed)
        host = created.get("host") if isinstance(created.get("host"), dict) else created
        host_id = host.get("id")
        self.assertTrue(host_id, created)
        goto_self_hosted(page, workspace_url)

        row = _list_row(page, seed)
        expect(row).to_be_visible()
        row.click()
        rename = page.get_by_title("Rename")
        expect(rename).to_have_text(seed)
        rename.click()
        inp = page.locator("input[autofocus]")
        expect(inp).to_be_visible()
        inp.fill(f"rename-fail-{uuid.uuid4().hex[:8]}")

        def _fail_patch(route) -> None:
            if route.request.method.upper() == "PATCH":
                route.fulfill(
                    status=500,
                    content_type="application/json",
                    body='{"error":"rename failed"}',
                )
                return
            route.continue_()

        page.route("**/api/workspaces/*/worker-hosts/*", _fail_patch)
        try:
            inp.press("Enter")
            expect(
                page.get_by_role("status").filter(has_text="rename failed")
            ).to_be_visible()
            expect(page.get_by_title("Rename")).to_have_text(seed)
        finally:
            page.unroute("**/api/workspaces/*/worker-hosts/*", _fail_patch)
        listed = _listed_host(page, ws, host_id)
        self.assertEqual(listed.get("name"), seed, listed)


if __name__ == "__main__":
    unittest.main(verbosity=2)
