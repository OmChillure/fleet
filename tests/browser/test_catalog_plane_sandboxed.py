#!/usr/bin/env python3
"""Layer 2 e2e: the API is not a runner (catalog:e2e-plane-runtime).

Self-hosted lists paired hosts only. Empty workspace shows No workers yet.
Fail closed — no skip.
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

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runners_e2e_helpers import (  # noqa: E402
    API,
    UI,
    assert_no_control_plane_card,
    goto_self_hosted,
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

    def test_empty_self_hosted_shows_no_workers_and_no_plane_card(self) -> None:
        # catalog:e2e-plane-runtime — API is not a runner; empty list + empty state.
        page = self.page
        _, workspace_url = _signup_api(page)

        goto_self_hosted(page, workspace_url)
        assert_no_control_plane_card(page)
        page.get_by_text("No workers yet", exact=True).wait_for(state="visible")
        page.get_by_text("The API is not a runner").wait_for(state="visible")


if __name__ == "__main__":
    unittest.main(verbosity=2)
