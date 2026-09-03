#!/usr/bin/env python3
"""Inbox page browser e2e: list → Start monitor → Finish.

Fail-closed: this-tree identity (`/health.git_sha` + UI `/fleet-git-sha.txt`
== `scripts/tree-id.sh`) and Playwright against :3030 / :8080. Missing stack
or a foreign Kind image is a fail, not a skip.

Empty inbox is not success for Finish. The page is seeded with a connected
github row (so Start monitor has a listen source) and an open ticket (so
Finish is a real button). Playwright still drives the chrome.

  python3 -m pytest tests/browser/test_inbox_e2e.py -q
  python3 -m unittest tests.browser.test_inbox_e2e -v
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
    last_err: Exception | None = None
    for _ in range(6):
        try:
            page.goto(f"{workspace_url}/inbox", wait_until="domcontentloaded")
        except Exception as e:  # noqa: BLE001 — dx restart / connection reset
            last_err = e
            time.sleep(2)
            continue
        wait_dx_ready(page)
        try:
            page.wait_for_selector("h1", state="attached", timeout=20_000)
            return
        except Exception as e:  # noqa: BLE001 — dx mid-rebuild; retry
            last_err = e
            time.sleep(2)
    raise RuntimeError(f"could not open inbox page: {last_err}")


def _json(res) -> dict:
    text = res.text()
    if not res.ok:
        raise RuntimeError(f"{res.status} {res.url}: {text[:500]}")
    if not text:
        return {}
    return json.loads(text)


def _inbox(page, workspace_id: str) -> dict:
    return _json(page.request.get(f"{UI}/api/workspaces/{workspace_id}/inbox"))


def _ticket(inbox: dict, provider: str, key: str) -> dict | None:
    for t in inbox.get("tickets") or []:
        if isinstance(t, dict) and t.get("provider") == provider and t.get("key") == key:
            return t
    return None


def _wait_inbox(page, workspace_id: str, timeout_s: float = 20.0) -> dict:
    deadline = time.time() + timeout_s
    last: dict = {}
    last_err: Exception | None = None
    while time.time() < deadline:
        try:
            last = _inbox(page, workspace_id)
            last_err = None
            return last
        except Exception as e:  # noqa: BLE001 — poll until list is readable
            last_err = e
        page.wait_for_timeout(400)
    raise AssertionError(f"GET inbox failed: last={last!r} err={last_err}"[:800])


def _wait_monitor_enabled(
    page, workspace_id: str, want: bool, timeout_s: float = 20.0
) -> dict:
    deadline = time.time() + timeout_s
    last: dict = {}
    while time.time() < deadline:
        last = _wait_inbox(page, workspace_id, timeout_s=5.0)
        mon = last.get("monitor") or {}
        if bool(mon.get("enabled")) is want:
            return last
        page.wait_for_timeout(400)
    raise AssertionError(
        f"inbox monitor.enabled {((last.get('monitor') or {}).get('enabled'))!r} "
        f"!= {want!r}: {last!r}"[:800]
    )


def _wait_ticket(
    page,
    workspace_id: str,
    provider: str,
    key: str,
    timeout_s: float = 20.0,
) -> dict:
    deadline = time.time() + timeout_s
    last: dict = {}
    while time.time() < deadline:
        last = _wait_inbox(page, workspace_id, timeout_s=5.0)
        hit = _ticket(last, provider, key)
        if hit is not None:
            return hit
        page.wait_for_timeout(400)
    raise AssertionError(
        f"inbox missing {provider}:{key}: {last!r}"[:800]
    )


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

    def test_unauthenticated_inbox_redirects_to_login(self) -> None:
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            fake = uuid.uuid4()
            page.goto(f"{UI}/{fake}/inbox", wait_until="domcontentloaded")
            page.wait_for_url("**/login", timeout=15_000)
            self.assertIn("/login", page.url)
        finally:
            ctx.close()

    def test_list_monitor_finish(self) -> None:
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            ws, workspace_url = signup_login(page, prefix="inbox-e2e")
            _goto_inbox(page, workspace_url)
            self.assertEqual(page.locator("h1").first.inner_text().strip(), "Inbox")
            self.assertIn("/inbox", page.url)
            nav = page.locator("aside nav")
            self.assertTrue(
                nav.get_by_role("link", name="Inbox", exact=True).count() > 0,
                "sidebar missing Inbox under Monitoring",
            )
            for name in ("Start monitor", "Stop monitor", "Refresh now"):
                self.assertTrue(
                    page.get_by_role("button", name=name).count() > 0,
                    f"Inbox page missing {name}",
                )
            body = page.inner_text("body")
            self.assertIn("No tickets yet. Connect integrations, then Start monitor.", body)
            self.assertIn("0 ticket(s)", body)
            listed = _wait_inbox(page, ws)
            self.assertEqual(listed.get("tickets") or [], [], listed)
            mon0 = listed.get("monitor")
            self.assertTrue(
                mon0 is None or mon0.get("enabled") is not True,
                f"fresh workspace must not already be monitoring: {listed}",
            )

            seed_connected_github(ws)
            start = page.get_by_role("button", name="Start monitor")
            self.assertTrue(start.first.is_enabled(), "Start monitor stayed disabled")
            start.first.click()
            after_start = _wait_monitor_enabled(page, ws, True)
            mon = after_start.get("monitor") or {}
            self.assertTrue(mon.get("enabled"), after_start)
            providers = [str(p).lower() for p in (mon.get("providers") or [])]
            self.assertIn("github", providers, after_start)
            page.get_by_text(re.compile(r"monitor on")).first.wait_for(timeout=15_000)
            stop = page.get_by_role("button", name="Stop monitor")
            self.assertTrue(stop.first.is_enabled(), "Stop monitor stayed disabled after start")

            key = f"inbox-e2e-{uuid.uuid4().hex[:10]}/board#1"
            title = f"e2e-inbox-{uuid.uuid4().hex[:10]}"
            seed_open_ticket(ws, "github", key, title)
            refresh = page.get_by_role("button", name="Refresh now")
            refresh.first.click()
            row = _wait_ticket(page, ws, "github", key)
            self.assertEqual(row.get("title"), title, row)
            self.assertNotIn(
                (row.get("state") or "").lower(),
                {"launched", "done", "ignored", "closed"},
                f"seeded ticket must still be open for Finish: {row}",
            )
            self.assertTrue(
                row.get("run_id") in (None, "", []),
                f"seeded ticket must not already have a run: {row}",
            )
            ticket_label = page.get_by_text(f"{key} · {title}", exact=False)
            deadline = time.time() + 20
            while time.time() < deadline:
                try:
                    if ticket_label.count() and ticket_label.first.is_visible():
                        break
                except Exception:
                    pass
                if refresh.first.is_enabled():
                    refresh.first.click()
                page.wait_for_timeout(500)
            ticket_label.first.wait_for(timeout=10_000)
            finish = page.get_by_role("button", name="Finish", exact=True)
            finish.first.wait_for(state="visible", timeout=15_000)
            self.assertTrue(finish.first.is_enabled(), "Finish stayed disabled")
            finish.first.click()

            deadline = time.time() + 25
            launched = row
            while time.time() < deadline:
                launched = _wait_ticket(page, ws, "github", key)
                rid = launched.get("run_id")
                state = (launched.get("state") or "").lower()
                if rid and state == "launched":
                    break
                page.wait_for_timeout(400)
            run_id = launched.get("run_id")
            self.assertTrue(run_id, launched)
            self.assertIsNotNone(_UUID_RE.fullmatch(str(run_id)), launched)
            self.assertEqual((launched.get("state") or "").lower(), "launched", launched)
            self.assertEqual(launched.get("title"), title, launched)
            self.assertEqual(launched.get("key"), key, launched)
            page.get_by_text(
                re.compile(r"run\s+" + re.escape(str(run_id)))
            ).first.wait_for(timeout=15_000)

            hide_deadline = time.time() + 15
            while time.time() < hide_deadline:
                if page.get_by_role("button", name="Finish", exact=True).count() == 0:
                    break
                page.wait_for_timeout(200)
            self.assertEqual(
                page.get_by_role("button", name="Finish", exact=True).count(),
                0,
                "Finish must hide on a launched inbox row",
            )

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
