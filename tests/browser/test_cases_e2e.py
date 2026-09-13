#!/usr/bin/env python3
"""Cases page browser e2e: create → observe → cancel; Complete chrome.

Fail-closed: this-tree identity (`/health.git_sha` + UI `/fleet-git-sha.txt`
== `scripts/tree-id.sh`) and Playwright against :3030 / :8080. Missing stack
or a foreign Kind image is a fail, not a skip.

  python3 -m pytest tests/browser/test_cases_e2e.py -q
  python3 -m pytest tests/browser/test_cases_e2e.py -q -k cases_decide_complete
  python3 -m unittest tests.browser.test_cases_e2e -v
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
from runners_e2e_helpers import UI, signup_login, wait_dx_ready  # noqa: E402
from stack_identity import ForeignStackError, require_this_tree  # noqa: E402

API = os.environ.get("FLEET_API_URL", "http://127.0.0.1:8080").rstrip("/")
REPO = Path(__file__).resolve().parents[2]
BROWSERS = os.environ.get(
    "PLAYWRIGHT_BROWSERS_PATH",
    str(REPO / ".tmp" / "ms-playwright"),
)
os.environ.setdefault("PLAYWRIGHT_BROWSERS_PATH", BROWSERS)

_CASE_ID_RE = re.compile(r"/cases/([0-9a-f-]{36})", re.I)


def _case_id_from_url(url: str) -> str | None:
    match = _CASE_ID_RE.search(url)
    return match.group(1) if match else None


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


def _goto_cases(page, workspace_url: str) -> None:
    last_err: Exception | None = None
    for _ in range(6):
        try:
            page.goto(f"{workspace_url}/cases", wait_until="domcontentloaded")
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
    raise RuntimeError(f"could not open cases page: {last_err}")


def _json(res) -> dict:
    text = res.text()
    if not res.ok:
        raise RuntimeError(f"{res.status} {res.url}: {text[:500]}")
    if not text:
        return {}
    return json.loads(text)


def _wait_case(page, workspace_id: str, case_id: str, timeout_s: float = 20.0) -> dict:
    deadline = time.time() + timeout_s
    last: dict = {}
    last_err: Exception | None = None
    while time.time() < deadline:
        try:
            last = _json(
                page.request.get(f"{UI}/api/workspaces/{workspace_id}/cases/{case_id}")
            )
            last_err = None
            if last.get("id") == case_id:
                return last
        except Exception as e:  # noqa: BLE001 — poll until detail exists
            last_err = e
        page.wait_for_timeout(400)
    raise AssertionError(
        f"case {case_id} not readable: last={last!r} err={last_err}"[:800]
    )


def _wait_status(
    page, workspace_id: str, case_id: str, want: str, timeout_s: float = 20.0
) -> dict:
    deadline = time.time() + timeout_s
    last: dict = {}
    while time.time() < deadline:
        last = _wait_case(page, workspace_id, case_id, timeout_s=5.0)
        if (last.get("status") or "") == want:
            return last
        page.wait_for_timeout(400)
    raise AssertionError(
        f"case {case_id} status {last.get('status')!r} != {want!r}: {last!r}"[:800]
    )


def _event_kinds(case: dict) -> list[str]:
    return [
        str(ev.get("kind") or "")
        for ev in (case.get("events") or [])
        if isinstance(ev, dict)
    ]


_OPEN_ACTIONS = ("Decide now", "Complete", "Fail", "Cancel", "Observe")


def _create_case_from_form(page, title: str, policy: str) -> str:
    """Create from the always-visible New case form.

    After the first create we are already on /cases/{uuid}. Playwright
    wait_for_url(regex) returns immediately on the current URL, so the
    second/third create must wait for a *different* case id.
    """
    prev = _case_id_from_url(page.url)
    title_box = page.locator("input[placeholder*='Title']")
    policy_box = page.locator("textarea[placeholder*='Policy']")
    title_box.first.wait_for(state="visible", timeout=15_000)
    policy_box.first.wait_for(state="visible", timeout=15_000)
    title_box.first.fill(title)
    policy_box.first.fill(policy)
    create = page.get_by_role("button", name="Create case")
    if not create.first.is_enabled():
        raise AssertionError("Create case stayed disabled")
    create.first.click()

    def _landed_on_new(url: str) -> bool:
        cid = _case_id_from_url(url)
        return cid is not None and cid != prev

    page.wait_for_url(_landed_on_new, timeout=20_000)
    cid = _case_id_from_url(page.url)
    if cid is None:
        raise AssertionError(f"create did not land on case detail: {page.url}")
    return cid


def _action_button(page, name: str):
    return page.get_by_role("button", name=name, exact=True)


def _wait_actions_gone(page, names: tuple[str, ...] = _OPEN_ACTIONS) -> None:
    from playwright.sync_api import expect

    for name in names:
        expect(_action_button(page, name)).to_have_count(0, timeout=15_000)


class CasesE2ETests(unittest.TestCase):
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

    def test_create_observe_cancel(self) -> None:
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            ws, workspace_url = signup_login(page, prefix="cases-e2e")
            _goto_cases(page, workspace_url)

            title = f"e2e-case-{uuid.uuid4().hex[:10]}"
            policy = (
                "Wait for a human observation, then stay open until cancelled. "
                "Do not complete on your own."
            )
            case_id = _create_case_from_form(page, title, policy)
            page.get_by_role("heading", name=title).first.wait_for(timeout=15_000)

            note = "customer asked for a refund on order 42"
            observe_box = page.locator("input[placeholder*='Observe']")
            observe_box.first.wait_for(state="visible", timeout=15_000)
            observe_box.first.fill(note)
            observe_btn = _action_button(page, "Observe")
            if not observe_btn.first.is_enabled():
                raise AssertionError("Observe stayed disabled")
            observe_btn.first.click()
            page.get_by_text("observed", exact=True).first.wait_for(timeout=15_000)

            _action_button(page, "Cancel").first.click()
            cancelled = _wait_status(page, ws, case_id, "cancelled")
            self.assertEqual(cancelled.get("status"), "cancelled", cancelled)
            _wait_actions_gone(page)

            leftover = _wait_case(page, ws, case_id)
            self.assertEqual(leftover.get("status"), "cancelled", leftover)
            self.assertIn("observed", _event_kinds(leftover), leftover)
            self.assertIn(note, json.dumps(leftover.get("events") or []), leftover)
        finally:
            ctx.close()

    def test_cases_decide_complete(self) -> None:
        from playwright.sync_api import expect

        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            ws, workspace_url = signup_login(page, prefix="cases-decide")
            _goto_cases(page, workspace_url)

            title = f"e2e-complete-{uuid.uuid4().hex[:10]}"
            case_id = _create_case_from_form(
                page,
                title,
                "Stay open until a human completes this case.",
            )
            page.get_by_role("heading", name=title).first.wait_for(timeout=15_000)
            created = _wait_case(page, ws, case_id)
            self.assertEqual(created.get("status"), "open", created)

            complete = _action_button(page, "Complete")
            expect(complete.first).to_be_enabled()
            complete.first.click()

            completed = _wait_status(page, ws, case_id, "completed")
            self.assertEqual(completed.get("status"), "completed", completed)
            expect(page.get_by_text("completed", exact=True).first).to_be_visible()
            _wait_actions_gone(page)
            row = page.locator("li").filter(has=page.get_by_text(title, exact=True))
            expect(row.get_by_text("completed", exact=True)).to_be_visible()
        finally:
            ctx.close()


if __name__ == "__main__":
    unittest.main(verbosity=2)
