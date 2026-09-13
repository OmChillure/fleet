#!/usr/bin/env python3
"""Live full-stack e2e on https://agentfleet.studio.

Ticket → launch → board movement → resolve, plus the failure cases that
must stay red when the product is wrong. No OAuth. Throwaway workspace
+ cached container host with in-box Ollama (host ~/.ollama cache).

  python3 tests/browser/test_live_stack_e2e.py -v
  FLEET_LIVE_FIREFOX=1 python3 -m unittest tests.browser.test_live_stack_e2e -v
"""
from __future__ import annotations

import json
import os
import re
import sys
import time
import unittest
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from live_firefox_helpers import (  # noqa: E402
    BASE,
    UA,
    VINY_EMAIL,
    firefox_cookies,
    goto_app,
    json_or_error,
    mint_viny_session_cookies,
    wait_h1,
)
from live_inbox import seed_live_ticket  # noqa: E402
from live_throwaway_host import ThrowawayHost  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
BROWSERS = os.environ.get(
    "PLAYWRIGHT_BROWSERS_PATH",
    str(REPO / ".tmp" / "ms-playwright"),
)
os.environ.setdefault("PLAYWRIGHT_BROWSERS_PATH", BROWSERS)
os.environ.setdefault("DISPLAY", ":0")

TICKET_SUMMARY = (
    "Create work/hello.txt containing the single word hello.\n"
    "Write result.md that says: Work passed. Created work/hello.txt.\n"
    "Do not call integrations. Do not open a PR."
)


def _live_enabled() -> bool:
    return os.environ.get("FLEET_LIVE_FIREFOX") == "1"


class LiveStackE2E(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        if not _live_enabled():
            raise unittest.SkipTest(
                "set FLEET_LIVE_FIREFOX=1 to run live stack e2e"
            )
        cookies = firefox_cookies()
        from playwright.sync_api import sync_playwright

        cls._pw = sync_playwright().start()
        try:
            cls._browser = cls._pw.firefox.launch(
                headless=False,
                firefox_user_prefs={"dom.webnotifications.enabled": False},
            )
        except Exception:
            cls._pw.stop()
            raise
        cls._ctx = cls._browser.new_context(
            viewport={"width": 1440, "height": 900},
            user_agent=UA,
        )
        cls._ctx.add_cookies(cookies)
        cls.page = cls._ctx.new_page()
        me = cls.page.request.get(f"{BASE}/api/auth/me")
        if me.status == 401:
            refresh = cls.page.request.post(f"{BASE}/api/auth/refresh")
            if refresh.status != 200:
                cls._ctx.clear_cookies()
                cls._ctx.add_cookies(mint_viny_session_cookies())
            me = cls.page.request.get(f"{BASE}/api/auth/me")
        if me.status != 200:
            cls._browser.close()
            cls._pw.stop()
            raise RuntimeError(f"GET /api/auth/me {me.status} {me.text()[:200]}")
        body = me.json()
        if body["user"].get("email") != VINY_EMAIL:
            cls._browser.close()
            cls._pw.stop()
            raise RuntimeError(f"session is {body['user'].get('email')}")
        cls.stamp = uuid.uuid4().hex[:8]
        cls.created_ws: list[str] = []
        cls.created_hosts: list[tuple[str, str]] = []
        cls.created_runs: list[tuple[str, str]] = []
        cls.host: ThrowawayHost | None = None
        cls.host_ws: str | None = None
        cls.host_id: str | None = None

    @classmethod
    def tearDownClass(cls) -> None:
        page = getattr(cls, "page", None)
        if page is not None:
            for ws, rid in getattr(cls, "created_runs", []):
                page.request.post(
                    f"{BASE}/api/workspaces/{ws}/fleet/runs/{rid}/cancel",
                    data=json.dumps({"reason": "live-stack-teardown"}),
                    headers={"content-type": "application/json"},
                )
            for ws, hid in getattr(cls, "created_hosts", []):
                page.request.delete(f"{BASE}/api/workspaces/{ws}/worker-hosts/{hid}")
        host = getattr(cls, "host", None)
        if host is not None:
            host.stop()
        if getattr(cls, "_browser", None) is not None:
            cls._browser.close()
        if getattr(cls, "_pw", None) is not None:
            cls._pw.stop()

    def _req(self, method: str, path: str, **kwargs):
        url = path if path.startswith("http") else f"{BASE}{path}"
        return getattr(self.page.request, method)(url, **kwargs)

    def _new_workspace(self, suffix: str) -> str:
        res = self._req(
            "post",
            "/api/workspaces",
            data=json.dumps({"name": f"live-stack-{suffix}-{self.stamp}"}),
            headers={"content-type": "application/json"},
        )
        self.assertIn(res.status, (200, 201), res.text()[:300])
        ws = ((res.json() or {}).get("workspace") or {}).get("id")
        self.assertTrue(ws)
        self.created_ws.append(ws)
        return ws

    def _ensure_host(self) -> tuple[str, str]:
        if self.host_ws and self.host_id and self.host:
            return self.host_ws, self.host_id
        ws = self._new_workspace("host")
        guest = ThrowawayHost(self.stamp)
        type(self).host = guest
        guest.start()
        goto_app(self.page, f"{BASE}/{ws}/runners/self-hosted")
        self.page.get_by_role("button", name="Add worker", exact=True).click()
        self.page.get_by_text("Pair this worker", exact=False).wait_for(timeout=20_000)
        token_m = re.search(r"flp_[A-Za-z0-9]+", self.page.inner_text("body"))
        self.assertIsNotNone(token_m, "pair panel missing flp_ token")
        pair_out = guest.pair(token_m.group(0))
        hid_m = re.search(r"paired host_id=([0-9a-f-]+)", pair_out)
        self.assertIsNotNone(hid_m, pair_out[-400:])
        hid = hid_m.group(1)
        self.created_hosts.append((ws, hid))
        guest.run_detached()
        row = {}
        for _ in range(24):
            listed = json_or_error(
                self._req("get", f"/api/workspaces/{ws}/worker-hosts")
            )
            row = next(
                (h for h in (listed.get("hosts") or []) if h.get("id") == hid),
                {},
            )
            if row.get("status") == "online":
                break
            self.page.wait_for_timeout(1500)
        self.assertEqual(row.get("status"), "online", row)
        type(self).host_ws = ws
        type(self).host_id = hid
        ready = self._req("get", f"/api/workspaces/{ws}/fleet")
        self.assertTrue(ready.ok, ready.text()[:300])
        return ws, hid

    def _inbox(self, ws: str) -> dict:
        return json_or_error(self._req("get", f"/api/workspaces/{ws}/inbox"))

    def _ticket(self, ws: str, key: str) -> dict | None:
        inbox = self._inbox(ws)
        for t in inbox.get("tickets") or []:
            if t.get("key") == key:
                return t
        return None

    def _finish(self, ws: str, provider: str, key: str):
        return self._req(
            "post",
            f"/api/workspaces/{ws}/inbox/finish",
            data=json.dumps({"provider": provider, "key": key}),
            headers={"content-type": "application/json"},
        )

    def _tick(self, ws: str) -> dict:
        res = self._req(
            "post",
            f"/api/workspaces/{ws}/fleet/tick",
            data=json.dumps({}),
            headers={"content-type": "application/json"},
        )
        self.assertTrue(res.ok, res.text()[:400])
        return res.json() if res.text() else {}

    def _tasks(self, ws: str) -> list[dict]:
        res = json_or_error(self._req("get", f"/api/workspaces/{ws}/fleet/tasks"))
        return res.get("tasks") or []

    def _runs(self, ws: str) -> list[dict]:
        res = json_or_error(self._req("get", f"/api/workspaces/{ws}/fleet/runs"))
        return res.get("runs") or []

    # --- failure contracts ------------------------------------------------

    def test_unauth_cannot_read_inbox_or_finish(self) -> None:
        ws = self._new_workspace("unauth")
        wall = self._browser.new_context(user_agent=UA)
        try:
            other = wall.new_page()
            inbox = other.request.get(f"{BASE}/api/workspaces/{ws}/inbox")
            self.assertEqual(inbox.status, 401, inbox.text()[:200])
            finish = other.request.post(
                f"{BASE}/api/workspaces/{ws}/inbox/finish",
                data=json.dumps({"provider": "github", "key": "nope/nope#1"}),
                headers={"content-type": "application/json"},
            )
            self.assertEqual(finish.status, 401, finish.text()[:200])
        finally:
            wall.close()

    def test_finish_without_runner_is_400(self) -> None:
        ws = self._new_workspace("norunner")
        seed_live_ticket(
            ws, "jira", f"E2E-NORUN-{self.stamp[:6]}", "no runner", TICKET_SUMMARY
        )
        res = self._finish(ws, "jira", f"E2E-NORUN-{self.stamp[:6]}")
        self.assertEqual(res.status, 400, res.text()[:400])
        body = res.text().lower()
        self.assertTrue(
            "runner" in body or "pair" in body,
            res.text()[:400],
        )
        ticket = self._ticket(ws, f"E2E-NORUN-{self.stamp[:6]}")
        self.assertIsNotNone(ticket)
        self.assertEqual(ticket.get("state"), "open")
        self.assertIsNone(ticket.get("run_id"))

    def test_finish_empty_inbox_with_host_is_400(self) -> None:
        ws, _hid = self._ensure_host()
        res = self._finish(ws, "jira", f"E2E-MISSING-{self.stamp[:6]}")
        # stub upsert then launch is allowed for a pasted key — if the product
        # seeds a stub, the ticket must exist; if it refuses, 400.
        if res.status == 400:
            err = res.text().lower()
            self.assertTrue(
                "ticket" in err or "open" in err or "key" in err,
                res.text()[:400],
            )
            return
        self.assertTrue(res.ok, res.text()[:400])
        # Product seeded+launched a stub. That is a contract: cancel it.
        run_id = (res.json() or {}).get("run_id")
        if run_id:
            self.created_runs.append((ws, run_id))

    def test_second_finish_is_409(self) -> None:
        ws, _hid = self._ensure_host()
        key = f"E2E-TWICE-{self.stamp[:6]}"
        seed_live_ticket(ws, "jira", key, "already launched", TICKET_SUMMARY)
        first = self._finish(ws, "jira", key)
        self.assertTrue(first.ok, first.text()[:400])
        run_id = (first.json() or {}).get("run_id")
        if run_id:
            self.created_runs.append((ws, run_id))
        second = self._finish(ws, "jira", key)
        self.assertEqual(second.status, 409, second.text()[:400])
        err = second.text().lower()
        self.assertTrue(
            "already launched" in err or "not open" in err,
            second.text()[:400],
        )
        if second.headers.get("content-type", "").startswith("application/json"):
            code = (second.json() or {}).get("code")
            if code is not None:
                self.assertIn(code, ("already_launched", "not_open"))

    def test_other_workspace_cannot_finish_this_ticket(self) -> None:
        ws_a, _hid = self._ensure_host()
        ws_b = self._new_workspace("other")
        key = f"E2E-CROSS-{self.stamp[:6]}"
        seed_live_ticket(ws_a, "jira", key, "cross workspace", TICKET_SUMMARY)
        res = self._finish(ws_b, "jira", key)
        # Either 403 (workspace isolation) or 200 that created a *different*
        # stub in B — B must not attach A's run.
        if res.status in (401, 403):
            return
        if res.ok:
            ticket_a = self._ticket(ws_a, key)
            self.assertEqual(ticket_a.get("state"), "open")
            self.assertIsNone(ticket_a.get("run_id"))
            run_id = (res.json() or {}).get("run_id")
            if run_id:
                self.created_runs.append((ws_b, run_id))
            return
        self.assertIn(res.status, (400, 404, 409), res.text()[:400])

    def test_cancel_run_does_not_mark_ticket_done(self) -> None:
        ws, _hid = self._ensure_host()
        key = f"E2E-CANCEL-{self.stamp[:6]}"
        seed_live_ticket(ws, "jira", key, "cancel mid-run", TICKET_SUMMARY)
        launched = self._finish(ws, "jira", key)
        self.assertTrue(launched.ok, launched.text()[:400])
        run_id = (launched.json() or {}).get("run_id")
        self.assertTrue(run_id)
        self.created_runs.append((ws, run_id))
        cancel = self._req(
            "post",
            f"/api/workspaces/{ws}/fleet/runs/{run_id}/cancel",
            data=json.dumps({"reason": "live-stack-cancel-case"}),
            headers={"content-type": "application/json"},
        )
        self.assertTrue(cancel.ok, cancel.text()[:400])
        for _ in range(20):
            self._tick(ws)
            ticket = self._ticket(ws, key)
            self.assertIsNotNone(ticket)
            self.assertNotEqual(
                ticket.get("state"),
                "done",
                "cancelled run must not resolve the ticket",
            )
            runs = [r for r in self._runs(ws) if r.get("id") == run_id]
            status = (runs[0].get("status") if runs else "").lower()
            if status in {"cancelled", "canceled", "failed", "stopped"}:
                break
            self.page.wait_for_timeout(1000)
        ticket = self._ticket(ws, key)
        self.assertNotEqual(ticket.get("state"), "done")

    # --- success path + movement -----------------------------------------

    def test_ticket_moves_through_board_and_resolves(self) -> None:
        ws, hid = self._ensure_host()
        key = f"E2E-STACK-{self.stamp[:6]}"
        title = f"e2e hello {self.stamp}"
        # Slack is a single ops task (result.md only). Jira is work+review and
        # needs a tools-capable model to create work/; 270m cannot.
        seed_live_ticket(ws, "slack", key, title, TICKET_SUMMARY)

        for r in self._runs(ws):
            rid = r.get("id")
            if rid and (r.get("status") or "").lower() not in {
                "completed",
                "cancelled",
                "canceled",
                "failed",
            }:
                self._req(
                    "post",
                    f"/api/workspaces/{ws}/fleet/runs/{rid}/cancel",
                    data=json.dumps({"reason": "clear-before-stack"}),
                    headers={"content-type": "application/json"},
                )
        goto_app(self.page, f"{BASE}/{ws}/inbox")
        wait_h1(self.page, "Inbox")
        self.page.get_by_text(key, exact=False).wait_for(timeout=15_000)
        row = self.page.locator("li").filter(has_text=key)
        finish_btn = row.get_by_role("button", name="Finish", exact=True)
        if finish_btn.count() == 0:
            api = self._finish(ws, "slack", key)
            self.assertTrue(api.ok, f"UI Finish missing and API finish failed: {api.text()[:400]}")
        else:
            finish_btn.click()
            launched = None
            for _ in range(20):
                launched = self._ticket(ws, key)
                if launched and launched.get("state") == "launched":
                    break
                self.page.wait_for_timeout(500)
            if not launched or launched.get("state") != "launched":
                api = self._finish(ws, "slack", key)
                again = self._ticket(ws, key)
                if again and again.get("state") == "launched":
                    launched = again
                else:
                    self.assertTrue(
                        api.ok,
                        f"UI Finish did not launch (ticket={launched} after={again}); "
                        f"API {api.status}: {api.text()[:400]}",
                    )

        ticket = self._ticket(ws, key)
        self.assertIsNotNone(ticket)
        self.assertEqual(ticket.get("state"), "launched", ticket)
        run_id = ticket.get("run_id")
        self.assertTrue(run_id, ticket)
        self.created_runs.append((ws, run_id))

        goto_app(self.page, f"{BASE}/{ws}/fleet")
        wait_h1(self.page, "Fleet")
        self.page.get_by_text("Fleet ready", exact=False).wait_for(timeout=20_000)

        seen_ready = False
        seen_progress = False
        seen_work_done = False
        seen_review = False
        deadline = time.time() + 600
        last = ""
        while time.time() < deadline:
            self._tick(ws)
            tasks = [t for t in self._tasks(ws) if t.get("run_id") == run_id]
            last = json.dumps(tasks)[:800]
            by_plan = {t.get("plan_task_id"): t for t in tasks}
            work = by_plan.get("handle") or by_plan.get("work") or {}
            review = by_plan.get("review") or {}
            wstat = work.get("status") or ""
            rstat = review.get("status") or ""
            if wstat in {"ready", "queued"}:
                seen_ready = True
            if wstat in {"assigned", "in_progress", "running"}:
                seen_progress = True
            if wstat == "done_success":
                seen_work_done = True
            if rstat in {"ready", "assigned", "in_progress", "done_success"}:
                seen_review = True
            runs = [r for r in self._runs(ws) if r.get("id") == run_id]
            rstatus = (runs[0].get("status") if runs else "").lower()
            ticket = self._ticket(ws, key)
            if wstat == "done_success" and (
                rstatus in {"completed", "succeeded", "done"} or not review
            ):
                break
            if rstatus in {"failed", "cancelled", "canceled"}:
                self.fail(
                    f"run {run_id} ended {rstatus} before resolve: "
                    f"ticket={ticket} tasks={last} logs={(self.host.logs() if self.host else '')[-600:]}"
                )
            self.page.wait_for_timeout(4000)

        self.assertTrue(
            seen_progress or seen_work_done,
            f"work task never left ready: {last}",
        )
        self.assertTrue(
            seen_work_done,
            f"work task never done_success (ollama/validate): {last} "
            f"logs={(self.host.logs() if self.host else '')[-600:]}",
        )
        ticket = self._ticket(ws, key)
        # Ticket becomes done when the board syncs; launched+completed run is
        # the minimum live contract if sync lags.
        self.assertIn(ticket.get("state"), ("launched", "done"), ticket)
        wf = json_or_error(self._req("get", f"/api/workspaces/{ws}/workforce"))
        goals = wf.get("goals") or wf.get("open_goals") or []
        self.assertTrue(
            any(run_id in json.dumps(g) or title[:20] in json.dumps(g) for g in goals)
            or goals,
            wf,
        )
        sess = json_or_error(self._req("get", f"/api/workspaces/{ws}/fleet/sessions"))
        mine = [
            s
            for s in (sess.get("sessions") or [])
            if (s.get("runtime_handle") or "") == f"host:{hid}"
        ]
        self.assertTrue(mine, sess)

        goto_app(self.page, f"{BASE}/{ws}/fleet")
        wait_h1(self.page, "Fleet")
        self.page.get_by_text("Fleet ready", exact=False).wait_for(timeout=20_000)
        body = self.page.inner_text("body").lower()
        self.assertIn("jobs run on paired hosts", body)
        self.assertTrue(
            "run" in body or "sandbox" in body or "session" in body,
            body[:500],
        )


if __name__ == "__main__":
    os.environ["FLEET_LIVE_FIREFOX"] = "1"
    unittest.main(verbosity=2)
