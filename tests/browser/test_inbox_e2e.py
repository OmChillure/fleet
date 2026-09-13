#!/usr/bin/env python3
"""Inbox page browser e2e: index → GitHub app → Start monitor → Start run.

Fail-closed: this-tree identity (`/health.git_sha` + UI `/fleet-git-sha.txt`
== `scripts/tree-id.sh`) and Playwright against :3030 / :8080. Missing stack
or a foreign Kind image is a fail, not a skip.

Empty inbox is not success for Start run. The page is seeded with a connected
github row (so Start monitor has a listen source) and an open ticket (so
Start run is a real button). Playwright still drives the chrome.

  python3 -m pytest tests/browser/test_inbox_e2e.py -q
  python3 -m unittest tests.browser.test_inbox_e2e -v
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
from inbox_e2e_helpers import seed_connected_github, seed_open_ticket  # noqa: E402
from runners_e2e_helpers import UI, signup_login, wait_dx_ready  # noqa: E402
from stack_identity import ForeignStackError, require_this_tree  # noqa: E402

API = os.environ.get("FLEET_API_URL", "http://127.0.0.1:8080").rstrip("/")
REPO = Path(__file__).resolve().parents[2]
BROWSERS = os.environ.get(
    "PLAYWRIGHT_BROWSERS_PATH",
    str(REPO / ".tmp" / "ms-playwright"),
)
os.environ.setdefault("PLAYWRIGHT_BROWSERS_PATH", BROWSERS)

_UUID_RE = re.compile(
    r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", re.I
)


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


def _goto_inbox(page, workspace_url: str) -> None:
    from playwright.sync_api import expect

    page.goto(f"{workspace_url}/inbox", wait_until="domcontentloaded")
    wait_dx_ready(page)
    expect(page.get_by_role("heading", name="Inbox", exact=True)).to_be_visible()


def _json(res) -> dict:
    text = res.text()
    if not res.ok:
        raise RuntimeError(f"{res.status} {res.url}: {text[:500]}")
    if not text:
        return {}
    return json.loads(text)


class InboxE2ETests(unittest.TestCase):
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

    @classmethod
    def tearDownClass(cls) -> None:
        cls._browser.close()
        cls._pw.stop()

    def test_list_monitor_finish(self) -> None:
        from playwright.sync_api import expect

        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            ws, workspace_url = signup_login(page, prefix="inbox-e2e")
            _goto_inbox(page, workspace_url)

            seed_connected_github(ws)
            start = page.get_by_role("button", name="Start monitor")
            expect(start).to_be_enabled()
            start.click()
            expect(page.get_by_role("button", name="Stop monitor")).to_be_enabled()

            page.get_by_role("link", name="GitHub", exact=True).first.click()
            expect(page.get_by_role("heading", name="GitHub", exact=True)).to_be_visible()
            expect(page.get_by_text("How to set up GitHub")).to_be_visible()
            body = page.inner_text("body")
            self.assertNotIn("{provider}", body)
            self.assertNotIn("not_in_channel", body)

            key = f"inbox-e2e-{uuid.uuid4().hex[:10]}/board#1"
            title = f"e2e-inbox-{uuid.uuid4().hex[:10]}"
            seed_open_ticket(ws, "github", key, title)
            refresh = page.get_by_role("button", name="Refresh now")
            expect(refresh).to_be_enabled()
            refresh.click()
            expect(page.get_by_text(title, exact=False)).to_be_visible()
            finish = page.get_by_role("button", name="Start run", exact=True)
            expect(finish).to_be_enabled()
            finish.click()
            expect(
                page.get_by_role("status").filter(has_text="Fleet launched")
            ).to_be_visible(timeout=20_000)
            expect(page.get_by_role("button", name="Start run", exact=True)).to_have_count(
                0, timeout=15_000
            )
            run_line = page.get_by_text(re.compile(r"run\s+" + _UUID_RE.pattern, re.I))
            expect(run_line).to_be_visible()
            found = _UUID_RE.search(run_line.inner_text())
            self.assertIsNotNone(found, run_line.inner_text())
            run_id = found.group(0)

            runs = _json(page.request.get(f"{UI}/api/workspaces/{ws}/fleet/runs"))
            matching = [
                r
                for r in (runs.get("runs") or [])
                if isinstance(r, dict) and r.get("id") == run_id
            ]
            self.assertEqual(len(matching), 1, runs)
            self.assertEqual(matching[0].get("source"), "inbox", matching[0])
        finally:
            ctx.close()


if __name__ == "__main__":
    unittest.main(verbosity=2)
