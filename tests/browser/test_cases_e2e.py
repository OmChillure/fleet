#!/usr/bin/env python3
"""Cases page browser e2e: create → observe → cancel; Complete/Fail set status.

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


def _assert_open_actions(page, names: tuple[str, ...] = _OPEN_ACTIONS) -> None:
    missing: list[str] = []
    for name in names:
        btn = _action_button(page, name)
        try:
            btn.first.wait_for(state="visible", timeout=15_000)
        except Exception:  # noqa: BLE001 — collect all missing labels
            missing.append(name)
            continue
        if btn.count() == 0:
            missing.append(name)
    if missing:
        raise AssertionError(f"open case detail missing {missing}")


def _wait_actions_gone(page, names: tuple[str, ...] = _OPEN_ACTIONS) -> None:
    deadline = time.time() + 15
    last: dict[str, int] = {}
    while time.time() < deadline:
        last = {name: _action_button(page, name).count() for name in names}
        if all(count == 0 for count in last.values()):
            return
        page.wait_for_timeout(200)
    raise AssertionError(f"terminal case still shows action buttons: {last}")


def _wait_detail_status_pill(page, title: str, status: str) -> None:
    """Detail heading is the title; the chip is `{status} · {stage}`."""
    page.get_by_role("heading", name=title).first.wait_for(timeout=15_000)
    page.locator("span").filter(
        has_text=re.compile(rf"^{re.escape(status)}\s*·", re.I)
    ).first.wait_for(timeout=15_000)


def _listed_case(page, workspace_id: str, case_id: str) -> dict:
    listed = _json(page.request.get(f"{UI}/api/workspaces/{workspace_id}/cases"))
    rows = listed.get("cases") or []
    row = next((c for c in rows if c.get("id") == case_id), None)
    if row is None:
        raise AssertionError(f"case {case_id} missing from list: {listed!r}"[:800])
    return row


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

    def test_unauthenticated_cases_redirects_to_login(self) -> None:
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            fake = uuid.uuid4()
            page.goto(f"{UI}/{fake}/cases", wait_until="domcontentloaded")
            page.wait_for_url("**/login", timeout=15_000)
            self.assertIn("/login", page.url)
        finally:
            ctx.close()

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
            self.assertEqual(page.locator("h1").first.inner_text().strip(), "Cases")
            self.assertTrue(
                page.get_by_role("button", name="Create case").count() > 0,
                "Cases page missing Create case",
            )
            self.assertIn("New case", page.inner_text("body"))

            title = f"e2e-case-{uuid.uuid4().hex[:10]}"
            policy = (
                "Wait for a human observation, then stay open until cancelled. "
                "Do not complete on your own."
            )
            title_box = page.locator("input[placeholder*='Title']")
            policy_box = page.locator("textarea[placeholder*='Policy']")
            title_box.first.wait_for(state="visible", timeout=15_000)
            policy_box.first.wait_for(state="visible", timeout=15_000)
            title_box.first.fill(title)
            policy_box.first.fill(policy)
            create = page.get_by_role("button", name="Create case")
            self.assertTrue(create.first.is_enabled(), "Create case stayed disabled")
            create.first.click()
            page.wait_for_url(_CASE_ID_RE, timeout=20_000)
            match = _CASE_ID_RE.search(page.url)
            self.assertIsNotNone(match, page.url)
            case_id = match.group(1)

            created = _wait_case(page, ws, case_id)
            self.assertEqual(created.get("title"), title, created)
            self.assertNotIn(
                created.get("status"),
                {"completed", "failed", "cancelled"},
                f"new case must still be open for observe/cancel: {created}",
            )
            kinds = _event_kinds(created)
            self.assertIn("created", kinds, created)

            page.get_by_role("heading", name=title).first.wait_for(timeout=15_000)
            observe_box = page.locator("input[placeholder*='Observe']")
            observe_box.first.wait_for(state="visible", timeout=15_000)
            note = "customer asked for a refund on order 42"
            observe_box.first.fill(note)
            observe_btn = page.get_by_role("button", name="Observe", exact=True)
            self.assertTrue(observe_btn.first.is_enabled(), "Observe stayed disabled")
            observe_btn.first.click()

            deadline = time.time() + 20
            observed = created
            while time.time() < deadline:
                observed = _wait_case(page, ws, case_id)
                if "observed" in _event_kinds(observed):
                    break
                page.wait_for_timeout(400)
            self.assertIn("observed", _event_kinds(observed), observed)
            payload = json.dumps(observed.get("events") or [])
            self.assertIn(note, payload, observed)
            page.get_by_text("observed", exact=True).first.wait_for(timeout=15_000)

            cancel = page.get_by_role("button", name="Cancel", exact=True)
            cancel.first.wait_for(state="visible", timeout=10_000)
            cancel.first.click()
            deadline = time.time() + 20
            cancelled = {}
            while time.time() < deadline:
                cancelled = _wait_case(page, ws, case_id)
                if cancelled.get("status") == "cancelled" and "cancelled" in _event_kinds(
                    cancelled
                ):
                    break
                page.wait_for_timeout(400)
            self.assertEqual(cancelled.get("status"), "cancelled", cancelled)
            self.assertIn("cancelled", _event_kinds(cancelled), cancelled)
            page.get_by_text(re.compile(r"cancelled", re.I)).first.wait_for(
                timeout=15_000
            )
            hide_deadline = time.time() + 15
            while time.time() < hide_deadline:
                if page.get_by_role("button", name="Cancel", exact=True).count() == 0:
                    break
                page.wait_for_timeout(200)
            self.assertEqual(
                page.get_by_role("button", name="Cancel", exact=True).count(),
                0,
                "Cancel must hide on a terminal case",
            )

            listed = _json(page.request.get(f"{UI}/api/workspaces/{ws}/cases"))
            rows = listed.get("cases") or []
            row = next((c for c in rows if c.get("id") == case_id), None)
            self.assertIsNotNone(row, listed)
            self.assertEqual(row.get("status"), "cancelled", row)
            self.assertEqual(row.get("title"), title, row)
            page.get_by_text(title, exact=True).first.wait_for(timeout=15_000)
        finally:
            ctx.close()

    def test_cases_decide_complete(self) -> None:
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            ws, workspace_url = signup_login(page, prefix="cases-decide")
            _goto_cases(page, workspace_url)
            self.assertEqual(page.locator("h1").first.inner_text().strip(), "Cases")

            done_title = f"e2e-complete-{uuid.uuid4().hex[:10]}"
            done_id = _create_case_from_form(
                page,
                done_title,
                "Stay open until a human completes this case.",
            )
            page.get_by_role("heading", name=done_title).first.wait_for(timeout=15_000)
            created = _wait_case(page, ws, done_id)
            self.assertEqual(created.get("title"), done_title, created)
            self.assertEqual(created.get("status"), "open", created)
            _assert_open_actions(page)

            complete = _action_button(page, "Complete")
            self.assertTrue(complete.first.is_enabled(), "Complete stayed disabled")
            complete.first.click()
            completed = _wait_status(page, ws, done_id, "completed")
            self.assertEqual(completed.get("status"), "completed", completed)
            _wait_detail_status_pill(page, done_title, "completed")
            self.assertIn("completed", page.inner_text("body").lower())
            _wait_actions_gone(page)
            done_row = _listed_case(page, ws, done_id)
            self.assertEqual(done_row.get("status"), "completed", done_row)
            self.assertEqual(done_row.get("title"), done_title, done_row)
            page.get_by_text(done_title, exact=True).first.wait_for(timeout=15_000)
            page.locator("li").filter(
                has=page.get_by_text(done_title, exact=True)
            ).get_by_text("completed", exact=True).first.wait_for(timeout=15_000)

            fail_title = f"e2e-fail-{uuid.uuid4().hex[:10]}"
            fail_id = _create_case_from_form(
                page,
                fail_title,
                "Stay open until a human fails this case.",
            )
            page.get_by_role("heading", name=fail_title).first.wait_for(timeout=15_000)
            opened = _wait_case(page, ws, fail_id)
            self.assertEqual(opened.get("status"), "open", opened)
            _assert_open_actions(page)

            fail = _action_button(page, "Fail")
            self.assertTrue(fail.first.is_enabled(), "Fail stayed disabled")
            fail.first.click()
            failed = _wait_status(page, ws, fail_id, "failed")
            self.assertEqual(failed.get("status"), "failed", failed)
            _wait_detail_status_pill(page, fail_title, "failed")
            _wait_actions_gone(page)
            fail_row = _listed_case(page, ws, fail_id)
            self.assertEqual(fail_row.get("status"), "failed", fail_row)
            self.assertEqual(fail_row.get("title"), fail_title, fail_row)
            page.get_by_text(fail_title, exact=True).first.wait_for(timeout=15_000)
            page.locator("li").filter(
                has=page.get_by_text(fail_title, exact=True)
            ).get_by_text("failed", exact=True).first.wait_for(timeout=15_000)

            still_done = _wait_case(page, ws, done_id)
            self.assertEqual(still_done.get("status"), "completed", still_done)
            still_done_row = _listed_case(page, ws, done_id)
            self.assertEqual(still_done_row.get("status"), "completed", still_done_row)
            self.assertEqual(still_done_row.get("title"), done_title, still_done_row)
            page.locator("li").filter(
                has=page.get_by_text(done_title, exact=True)
            ).get_by_text("completed", exact=True).first.wait_for(timeout=15_000)

            open_title = f"e2e-open-{uuid.uuid4().hex[:10]}"
            open_id = _create_case_from_form(
                page,
                open_title,
                "Stay open. Do not complete on your own.",
            )
            page.get_by_role("heading", name=open_title).first.wait_for(timeout=15_000)
            still_open = _wait_case(page, ws, open_id)
            self.assertEqual(still_open.get("status"), "open", still_open)
            _assert_open_actions(page, ("Decide now", "Complete"))
            self.assertEqual(
                _wait_case(page, ws, done_id).get("status"),
                "completed",
                "third create must not rewrite the completed case",
            )
            self.assertEqual(
                _wait_case(page, ws, fail_id).get("status"),
                "failed",
                "third create must not rewrite the failed case",
            )
        finally:
            ctx.close()


if __name__ == "__main__":
    unittest.main(verbosity=2)
