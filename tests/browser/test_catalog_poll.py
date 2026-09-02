#!/usr/bin/env python3
"""Layer 2 e2e: Self-hosted list poll (~10s) after pair / heartbeat.

catalog:e2e-poll — Poll list ~10s. Card updates after pair / heartbeat.
Card text must appear from the in-page poll, not from a reload.
"""
from __future__ import annotations

import json
import os
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
    heartbeat,
    pair_host,
)
from stack_identity import ForeignStackError, require_this_tree  # noqa: E402

API = os.environ.get("FLEET_API_URL", "http://127.0.0.1:8080").rstrip("/")
REPO = Path(__file__).resolve().parents[2]
BROWSERS = os.environ.get(
    "PLAYWRIGHT_BROWSERS_PATH",
    str(REPO / ".tmp" / "ms-playwright"),
)
os.environ.setdefault("PLAYWRIGHT_BROWSERS_PATH", BROWSERS)

# page.rs POLL_MS = 10_000; allow one full cycle plus slack.
POLL_WAIT_MS = 20_000


def _signup_login_api(page) -> tuple[str, str]:
    """Cookie session via the UI origin. Avoids WASM signup under dx rebuild."""
    email = f"catalog-poll-{uuid.uuid4().hex[:12]}@example.test"
    password = "e2e-pass-ok"
    headers = {"Content-Type": "application/json"}
    created = page.request.post(
        f"{UI}/api/auth/signup",
        data=json.dumps(
            {
                "email": email,
                "password": password,
                "display_name": "E2E catalog-poll",
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


def _wait_dx_ready(page) -> None:
    """dx serve may show a rebuild toast over a still-live WASM app.

    Only block when the Self-hosted chrome is missing (true full rebuild).
    A leftover toast must not burn 90s — the poll loop is already running.
    """
    page.wait_for_timeout(400)
    h1 = page.locator("h1")
    if h1.count() and "Self-hosted" in (h1.first.inner_text() or ""):
        return
    banner = page.get_by_text("Your app is being rebuilt")
    if not banner.count():
        return
    try:
        if banner.first.is_visible():
            banner.first.wait_for(state="hidden", timeout=90_000)
    except Exception as e:
        raise RuntimeError(
            "UI still showing 'Your app is being rebuilt' after 90s and "
            "Self-hosted chrome never mounted. Rebuild host dx from this "
            f"tree (./scripts/human-test.sh launch) and re-run. {e}. "
            f"body={page.inner_text('body')[:800]!r}"
        ) from e


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


class CatalogPollTests(unittest.TestCase):
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

    def test_card_updates_after_pair_heartbeat(self) -> None:
        # catalog:e2e-poll — Poll list ~10s. Card updates after pair / heartbeat.
        page = self.page
        ws, workspace_url = _signup_login_api(page)
        goto_self_hosted(page, workspace_url)
        _wait_dx_ready(page)
        try:
            page.get_by_role("button", name="Add worker").wait_for(timeout=30_000)
        except Exception as e:
            raise AssertionError(
                f"Self-hosted page missing after landing: {e}. "
                f"url={page.url} body={page.inner_text('body')[:1200]!r}"
            ) from e
        # First list_workers_page has landed. Stay on this URL.
        self.assertIn("/runners/self-hosted", page.url)
        url_before = page.url

        suffix = uuid.uuid4().hex[:12]
        hostname = f"poll-box-{suffix}"
        name = f"poll-host-{suffix}"
        self.assertNotIn(hostname, page.inner_text("body"))

        created = create_host(page, ws, name)
        token = created["pair_token"]
        paired = pair_host(
            page,
            token,
            hostname=hostname,
            platform="linux-x86_64",
        )
        heartbeat(
            page,
            paired["machine_token"],
            {
                "hostname": hostname,
                "platform": "linux-x86_64",
                "hardware": {
                    "cpu_model": "Poll E2E CPU",
                    "cpu_cores": 2,
                    "os": "linux 6.8 x86_64",
                },
            },
        )

        # Pairing went through the API only — chrome must not have the card yet.
        self.assertEqual(page.url, url_before, "must not navigate away to pair")
        self.assertNotIn(
            hostname,
            page.inner_text("body"),
            "hostname must not appear before the ~10s list poll",
        )
        # Overlay after this point would freeze the poll loop; wait it out
        # before looking for the card (do not re-check absence — poll may fire).
        _wait_dx_ready(page)

        # Hostname is rendered in the same <p> as " · platform", so do not
        # wait on get_by_text(..., exact=True).
        card = page.locator("article").filter(has_text=hostname)
        try:
            card.first.wait_for(timeout=POLL_WAIT_MS)
        except Exception as e:
            raise AssertionError(
                f"card with hostname {hostname!r} did not appear from the "
                f"~10s list poll: {e}. body={page.inner_text('body')[:1200]!r}"
            ) from e
        self.assertEqual(
            page.url,
            url_before,
            "card must appear from the poll, not a navigation/reload",
        )
        self.assertGreater(card.count(), 0, "paired host card missing after poll")
        text = card.first.inner_text()
        self.assertIn(name, text)
        self.assertIn("online", text.lower())
        self.assertRegex(text.lower(), r"last seen (just now|\d+s ago|\d+m ago)")
        self.assertIn("linux-x86_64", text)


if __name__ == "__main__":
    unittest.main()
