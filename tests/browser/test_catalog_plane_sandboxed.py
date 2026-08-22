#!/usr/bin/env python3
"""Layer 2 e2e: plane runtime is SANDBOX_BACKEND (catalog:e2e-plane-runtime).

No Native/Sandboxed isolation checkboxes. The plane card shows the workspace
SANDBOX_BACKEND. Fail closed — no skip.
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
    list_hosts,
    wait_dx_ready,
)
from stack_identity import ForeignStackError, require_this_tree  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
BROWSERS = os.environ.get(
    "PLAYWRIGHT_BROWSERS_PATH",
    str(REPO / ".tmp" / "ms-playwright"),
)
os.environ.setdefault("PLAYWRIGHT_BROWSERS_PATH", BROWSERS)

HINT = "not implemented yet"


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


def _signup_api(page) -> tuple[str, str]:
    """Cookie session via API so a stuck dx rebuild banner cannot hang signup."""
    email = f"catalog-plane-sbx-{uuid.uuid4().hex[:12]}@example.test"
    password = "e2e-pass-ok"
    headers = {"Content-Type": "application/json"}
    created = page.request.post(
        f"{UI}/api/auth/signup",
        data=json.dumps(
            {
                "email": email,
                "password": password,
                "display_name": "E2E catalog-plane-sbx",
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


def _plane_row(page, workspace_id: str) -> dict[str, Any]:
    listed = list_hosts(page, workspace_id)
    hosts = listed.get("hosts") or []
    if not hosts:
        raise AssertionError(f"empty host list: {listed!r}"[:800])
    row = hosts[0]
    if row.get("kind") != "control_plane":
        raise AssertionError(f"first host is not the plane: {row!r}"[:800])
    return row


def _plane_card(page):
    first = page.locator("article").first
    first.wait_for(timeout=15_000)
    text = first.inner_text()
    if "Control plane" not in text:
        raise AssertionError(f"first card is not Control plane: {text[:400]!r}")
    return first


def _checkbox(card, name: str):
    # Accessible name for Sandboxed includes the placeholder hint.
    box = card.get_by_role("checkbox", name=re.compile(rf"^{re.escape(name)}($|\b)"))
    if box.count() == 0:
        box = card.locator("label").filter(has_text=re.compile(rf"^{re.escape(name)}($|\b)")).locator(
            "input"
        )
    if box.count() == 0:
        box = card.locator("label").filter(has_text=name).locator("input")
    return box


def _apply_if_visible(card, page) -> None:
    apply_btn = card.get_by_role("button", name="Apply changes")
    for _ in range(25):
        if apply_btn.count() and apply_btn.first.is_visible():
            apply_btn.first.click(force=True)
            apply_btn.first.wait_for(state="hidden", timeout=10_000)
            return
        page.wait_for_timeout(200)
    raise AssertionError("Apply changes never appeared after toggling plane Sandboxed")


class CatalogPlaneSandboxedToggleTests(unittest.TestCase):
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

    def test_toggle_sandboxed_on_the_plane_card(self) -> None:
        # catalog:e2e-plane-runtime — plane uses SANDBOX_BACKEND, no isolation UI.
        page = self.page
        _ws, workspace_url = _signup_api(page)

        goto_self_hosted(page, workspace_url)
        card = _plane_card(page)
        text = card.inner_text()
        self.assertIn("this process", text.lower())
        self.assertIn("Runtime:", text)
        self.assertIn("SANDBOX_BACKEND", text)
        self.assertNotIn("not implemented yet", text.lower())
        self.assertNotIn(
            "pending apply",
            text.lower(),
            "fresh plane must not show pending apply",
        )
        self.assertEqual(
            card.get_by_role("checkbox", name=re.compile(r"^Native$")).count(),
            0,
            "plane must not show Native isolation",
        )
        self.assertEqual(
            card.locator("label").filter(has_text="Sandboxed").count(),
            0,
            "plane must not show Sandboxed isolation",
        )

        row = _plane_row(page, _ws)
        self.assertEqual(row.get("kind"), "control_plane", row)
        self.assertEqual(row.get("name"), "Control plane", row)


if __name__ == "__main__":
    unittest.main(verbosity=2)
