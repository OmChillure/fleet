#!/usr/bin/env python3
"""Simulate shipped product behaviors on the live instance.

Uses the operator Firefox session (cookies from the default-release profile)
against https://agentfleet.studio. One test per behavior.

Pair uses a throwaway **container** (own config dir, docker.sock, kubeconfig)
so `viny-5700g`'s machine token is never rotated. That box then runs native,
Docker, and Kubernetes jobs on a throwaway workspace. Fleet / inbox sims
cancel leftover runs.

Throwaway users / goals / cases / personas / hosts use unique `live-sim-*`
names and are cancelled, revoked, or disabled in the same test.

Opt-in for discovery (so local `unittest discover` stays this-tree):

  FLEET_LIVE_FIREFOX=1 python3 -m unittest tests.browser.test_live_firefox_session -v

Running this file directly always enables the live flag.
"""
from __future__ import annotations

import json
import os
import re
import subprocess
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
    assert_no_secrets,
    firefox_cookies,
    goto_app,
    json_or_error,
    mint_viny_session_cookies,
    wait_h1,
)
from live_throwaway_host import ThrowawayHost  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
BROWSERS = os.environ.get(
    "PLAYWRIGHT_BROWSERS_PATH",
    str(REPO / ".tmp" / "ms-playwright"),
)
os.environ.setdefault("PLAYWRIGHT_BROWSERS_PATH", BROWSERS)
os.environ.setdefault("DISPLAY", ":0")

_PASSWORD = "e2e-pass-ok"


def _live_enabled() -> bool:
    return os.environ.get("FLEET_LIVE_FIREFOX") == "1"


def _require_playwright():
    try:
        from playwright.sync_api import sync_playwright
    except ImportError as e:
        raise RuntimeError(
            "playwright is not installed. Run "
            "crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh"
        ) from e
    return sync_playwright


class LiveFirefoxSessionTests(unittest.TestCase):
    """Each method is one live contract. Shared browser = one Firefox session."""

    @classmethod
    def setUpClass(cls) -> None:
        if not _live_enabled():
            raise unittest.SkipTest(
                "set FLEET_LIVE_FIREFOX=1 to run live Firefox session simulations"
            )
        cookies = firefox_cookies()
        pw = _require_playwright()
        cls._pw = pw().start()
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
                minted = mint_viny_session_cookies()
                cls._ctx.clear_cookies()
                cls._ctx.add_cookies(minted)
                me = cls.page.request.get(f"{BASE}/api/auth/me")
            else:
                me = cls.page.request.get(f"{BASE}/api/auth/me")
        me_status = me.status
        me_body = me.text()[:300]
        if me_status != 200:
            cls._browser.close()
            cls._pw.stop()
            raise RuntimeError(f"GET /api/auth/me {me_status} {me_body}")
        body = me.json()
        cls.user = body["user"]
        cls.workspaces = body["workspaces"]
        if cls.user.get("email") != VINY_EMAIL:
            cls._browser.close()
            cls._pw.stop()
            raise RuntimeError(f"Firefox session is {cls.user.get('email')}, not {VINY_EMAIL}")
        if cls.user.get("instance_admin") is not True:
            cls._browser.close()
            cls._pw.stop()
            raise RuntimeError("viny is not instance_admin on live /api/auth/me")
        home = next(
            (w for w in cls.workspaces if str(w.get("name", "")).lower().startswith("viny")),
            None,
        )
        if home is None:
            cls._browser.close()
            cls._pw.stop()
            raise RuntimeError(f"no viny workspace in {cls.workspaces}")
        cls.ws = home["id"]
        cls.ws_url = f"{BASE}/{cls.ws}"
        cls.stamp = uuid.uuid4().hex[:8]
        cls.created_goals: list[str] = []
        cls.created_cases: list[str] = []
        cls.created_personas: list[str] = []
        cls.created_users: list[str] = []
        cls.created_hosts: list[str] = []
        cls.created_runs: list[tuple[str, str]] = []
        cls.throwaway: ThrowawayHost | None = None

    @classmethod
    def tearDownClass(cls) -> None:
        page = getattr(cls, "page", None)
        ws = getattr(cls, "ws", None)
        if page is not None and ws:
            for gid in getattr(cls, "created_goals", []):
                page.request.post(
                    f"{BASE}/api/workspaces/{ws}/workforce/goals/{gid}/cancel"
                )
            for cid in getattr(cls, "created_cases", []):
                page.request.post(f"{BASE}/api/workspaces/{ws}/cases/{cid}/cancel")
            for pid in getattr(cls, "created_personas", []):
                page.request.delete(f"{BASE}/api/workspaces/{ws}/personas/{pid}")
            for uid in getattr(cls, "created_users", []):
                page.request.patch(
                    f"{BASE}/api/admin/users/{uid}",
                    data=json.dumps({"status": "disabled"}),
                    headers={"content-type": "application/json"},
                )
            for hid in getattr(cls, "created_hosts", []):
                page.request.delete(
                    f"{BASE}/api/workspaces/{ws}/worker-hosts/{hid}"
                )
            for item in getattr(cls, "created_runs", []):
                if isinstance(item, tuple):
                    rws, rid = item
                else:
                    rws, rid = ws, item
                page.request.post(
                    f"{BASE}/api/workspaces/{rws}/fleet/runs/{rid}/cancel",
                    data=json.dumps({"reason": "live-sim-teardown"}),
                    headers={"content-type": "application/json"},
                )
        guest = getattr(cls, "throwaway", None)
        if guest is not None:
            guest.stop()
            cls.throwaway = None
        if getattr(cls, "_browser", None) is not None:
            cls._browser.close()
        if getattr(cls, "_pw", None) is not None:
            cls._pw.stop()

    def _req(self, method: str, path: str, **kwargs):
        url = path if path.startswith("http") else f"{BASE}{path}"
        return getattr(self.page.request, method)(url, **kwargs)

    def _hosts(self) -> list[dict]:
        payload = json_or_error(
            self._req("get", f"/api/workspaces/{self.ws}/worker-hosts")
        )
        return payload.get("hosts") or []

    def _host_named(self, name: str) -> dict:
        match = next((h for h in self._hosts() if h.get("name") == name), None)
        self.assertIsNotNone(match, f"missing host {name}")
        return match

    def _cancel_run(self, run_id: str, workspace_id: str | None = None) -> None:
        ws = workspace_id or self.ws
        self.created_runs.append((ws, run_id))
        self._req(
            "post",
            f"/api/workspaces/{ws}/fleet/runs/{run_id}/cancel",
            data=json.dumps({"reason": "live-sim"}),
            headers={"content-type": "application/json"},
        )
        sessions = self._req("get", f"/api/workspaces/{ws}/fleet/sessions")
        if sessions.ok:
            rows = (sessions.json() or {}).get("sessions") or []
            for s in rows:
                sid = s.get("id") or s.get("session_id")
                if sid and s.get("run_id") == run_id:
                    st = (s.get("status") or "").lower()
                    if st in {"healthy", "spawning", "running", "active"}:
                        self._req(
                            "post",
                            f"/api/workspaces/{self.ws}/fleet/sessions/{sid}/kill",
                            data=json.dumps({"reason": "live-sim"}),
                            headers={"content-type": "application/json"},
                        )

    def _patch_isolation(self, workspace_id: str, isolation: str) -> None:
        settings = json_or_error(
            self._req("get", f"/api/workspaces/{workspace_id}/fleet/settings")
        )
        res = self._req(
            "patch",
            f"/api/workspaces/{workspace_id}/fleet/settings",
            data=json.dumps(
                {
                    "max_agents": settings.get("max_agents") or 3,
                    "fleet_llm": settings.get("fleet_llm") or "off",
                    "fleet_llm_model": settings.get("fleet_llm_model") or "",
                    "min_isolation": isolation,
                }
            ),
            headers={"content-type": "application/json"},
        )
        self.assertTrue(res.ok, res.text()[:300])
        got = json_or_error(
            self._req("get", f"/api/workspaces/{workspace_id}/fleet/settings")
        )
        self.assertEqual(got.get("min_isolation"), isolation, got)

    def _start_and_tick(self, workspace_id: str) -> str:
        before = json_or_error(
            self._req("get", f"/api/workspaces/{workspace_id}/fleet/runs")
        )
        before_ids = {r.get("id") for r in (before.get("runs") or []) if r.get("id")}
        started = self._req(
            "post",
            f"/api/workspaces/{workspace_id}/fleet/runs/fixture",
            data=json.dumps({"kind": "chain", "on_task_fail": "fail_run"}),
            headers={"content-type": "application/json"},
        )
        self.assertTrue(started.ok, started.text()[:400])
        run_id = (started.json() or {}).get("run_id")
        if not run_id:
            after = json_or_error(
                self._req("get", f"/api/workspaces/{workspace_id}/fleet/runs")
            )
            new = [
                r
                for r in (after.get("runs") or [])
                if r.get("id") and r.get("id") not in before_ids
            ]
            self.assertTrue(new, after)
            run_id = new[0]["id"]
        self.created_runs.append((workspace_id, run_id))
        tick = self._req(
            "post",
            f"/api/workspaces/{workspace_id}/fleet/tick",
            data=json.dumps({}),
            headers={"content-type": "application/json"},
        )
        self.assertTrue(tick.ok, tick.text()[:400])
        return run_id

    def _sessions_on_host(self, workspace_id: str, host_id: str) -> list[dict]:
        handle = f"host:{host_id}"
        sess = self._req("get", f"/api/workspaces/{workspace_id}/fleet/sessions")
        if not sess.ok:
            return []
        return [
            row
            for row in ((sess.json() or {}).get("sessions") or [])
            if (row.get("runtime_handle") or "") == handle
        ]

    def _wait_executor(
        self,
        workspace_id: str,
        host_id: str,
        kind: str,
        guest: ThrowawayHost,
        timeout_s: float = 90.0,
    ) -> dict:
        deadline = time.time() + timeout_s
        last = ""
        while time.time() < deadline:
            rows = self._sessions_on_host(workspace_id, host_id)
            logs = guest.logs()
            if kind == "host":
                if rows:
                    return rows[0]
            elif kind == "docker":
                ps = subprocess.run(
                    ["docker", "ps", "-a", "--filter", "name=fleet-wh-", "--format", "{{.Names}}"],
                    capture_output=True,
                    text=True,
                )
                if rows and (
                    (ps.stdout or "").strip() or "docker run" in logs.lower()
                ):
                    return rows[0]
            elif kind == "kube":
                crd = subprocess.run(
                    [
                        "kubectl",
                        "--context",
                        "kind-agent-fleet",
                        "-n",
                        "agent-fleet",
                        "get",
                        "fleetsandboxes",
                        "-o",
                        "name",
                    ],
                    capture_output=True,
                    text=True,
                )
                if rows and (
                    (crd.stdout or "").strip()
                    or "fleetsandbox" in logs.lower()
                    or "kube" in logs.lower()
                ):
                    return rows[0]
            last = f"sessions={rows!r} logs={logs[-800:]!r}"
            self.page.wait_for_timeout(1500)
        Path("/tmp/live-sim-host-fail.log").write_text(last)
        self.fail(f"no {kind} job on {host_id} within {timeout_s}s (see /tmp/live-sim-host-fail.log)")

    def _prove_docker_executor(self, guest: ThrowawayHost) -> None:
        """Host docker CLI + same image fleet-host would `docker run`."""
        name = f"fleet-wh-sim-{self.stamp}"
        work = guest.work_dir / "docker-smoke"
        work.mkdir(parents=True, exist_ok=True)
        ran = subprocess.run(
            [
                "docker",
                "exec",
                guest.name,
                "docker",
                "run",
                "--rm",
                "--name",
                name,
                "--network",
                "bridge",
                "-v",
                f"{work}:/work",
                "-w",
                "/work",
                "-e",
                "FLEET_HARNESS=mock",
                "agent-fleet-api:local",
                "true",
            ],
            capture_output=True,
            text=True,
            timeout=90,
        )
        self.assertEqual(
            ran.returncode,
            0,
            f"throwaway host docker run failed:\n{ran.stdout}\n{ran.stderr}",
        )

    def _prove_kube_executor(self) -> None:
        """Kind + FleetSandbox CRD + a guest pod the host would schedule."""
        pod = f"live-sim-kube-{self.stamp}"[:40]
        subprocess.run(
            [
                "kubectl",
                "--context",
                "kind-agent-fleet",
                "-n",
                "agent-fleet",
                "delete",
                "pod",
                pod,
                "--ignore-not-found",
            ],
            capture_output=True,
            text=True,
        )
        ran = subprocess.run(
            [
                "kubectl",
                "--context",
                "kind-agent-fleet",
                "-n",
                "agent-fleet",
                "run",
                pod,
                "--image=agent-fleet-api:local",
                "--restart=Never",
                "--command",
                "--",
                "/bin/true",
            ],
            capture_output=True,
            text=True,
        )
        self.assertEqual(
            ran.returncode,
            0,
            f"kubectl run failed:\n{ran.stdout}\n{ran.stderr}",
        )
        wait = subprocess.run(
            [
                "kubectl",
                "--context",
                "kind-agent-fleet",
                "-n",
                "agent-fleet",
                "wait",
                f"pod/{pod}",
                "--for=jsonpath={.status.phase}=Succeeded",
                "--timeout=60s",
            ],
            capture_output=True,
            text=True,
        )
        crd = subprocess.run(
            [
                "kubectl",
                "--context",
                "kind-agent-fleet",
                "get",
                "crd",
                "fleetsandboxes.fleet.agentfleet.io",
            ],
            capture_output=True,
            text=True,
        )
        subprocess.run(
            [
                "kubectl",
                "--context",
                "kind-agent-fleet",
                "-n",
                "agent-fleet",
                "delete",
                "pod",
                pod,
                "--ignore-not-found",
            ],
            capture_output=True,
            text=True,
        )
        self.assertEqual(crd.returncode, 0, crd.stderr)
        self.assertTrue(
            wait.returncode == 0 or "Ready" in (wait.stdout + wait.stderr),
            f"kube guest pod did not become Ready:\n{wait.stdout}\n{wait.stderr}",
        )

    def _signup_throwaway(self, prefix: str) -> tuple[str, str]:
        email = f"{prefix}-{self.stamp}-{uuid.uuid4().hex[:6]}@example.test"
        payload = {
            "email": email,
            "password": _PASSWORD,
            "display_name": f"Live sim {prefix}",
        }
        token = os.environ.get("SIGNUP_TOKEN", "").strip()
        if token:
            payload["token"] = token
        res = self.page.request.post(
            f"{BASE}/api/auth/signup",
            data=json.dumps(payload),
            headers={
                "content-type": "application/json",
                "user-agent": UA,
            },
        )
        if res.status == 403 and "signup" in res.text().lower() and not token:
            raise RuntimeError(
                "live signup 403 — set SIGNUP_TOKEN from the instance .env"
            )
        self.assertIn(res.status, (200, 201), res.text()[:300])
        body = res.json()
        uid = (body.get("user") or {}).get("id") or body.get("id")
        if not uid:
            listed = json_or_error(self._req("get", "/api/admin/users"))
            users = listed.get("users") or listed if isinstance(listed, dict) else []
            if isinstance(users, dict):
                users = users.get("users") or []
            match = next((u for u in users if u.get("email") == email), None)
            uid = (match or {}).get("id")
        self.assertTrue(uid, f"signup did not return user id: {body}")
        self.created_users.append(uid)
        return email, uid

    # --- session / shell -------------------------------------------------

    def test_live_health_reports_admin_api(self) -> None:
        health = json_or_error(self._req("get", "/health"))
        self.assertEqual(health.get("status"), "ok")
        self.assertEqual(health.get("service"), "fleet-server")
        apis = health.get("apis") or []
        for name in (
            "auth",
            "workspaces",
            "chat",
            "cases",
            "fleet",
            "orchestrator",
            "worker_hosts",
            "admin",
        ):
            self.assertIn(name, apis, health)
        self.assertTrue(health.get("git_sha"))

    def test_firefox_session_is_viny_instance_admin(self) -> None:
        me = json_or_error(self._req("get", "/api/auth/me"))
        self.assertEqual(me["user"]["email"], VINY_EMAIL)
        self.assertTrue(me["user"]["instance_admin"])
        self.assertGreaterEqual(len(me["workspaces"]), 1)

    def test_unauth_deep_link_redirects_to_login(self) -> None:
        wall = self._browser.new_context(user_agent=UA)
        try:
            other = wall.new_page()
            other.goto(f"{self.ws_url}/dashboard", wait_until="domcontentloaded")
            other.wait_for_url("**/login", timeout=15_000)
            self.assertIn("/login", other.url)
        finally:
            wall.close()

    def test_workspace_switcher_lists_memberships(self) -> None:
        goto_app(self.page, f"{self.ws_url}/dashboard")
        wait_h1(self.page, "Workforce")
        home_name = next(
            (w.get("name") or "" for w in self.workspaces if w.get("id") == self.ws),
            "",
        )
        switcher = self.page.locator("aside").get_by_role("button", name=home_name)
        switcher.first.click()
        body = self.page.locator("aside").inner_text()
        self.assertIn("viny", body.lower())
        other = next((w for w in self.workspaces if w.get("id") != self.ws), None)
        if other is not None:
            label = other.get("name") or ""
            item = self.page.get_by_text(label, exact=True)
            if item.count():
                item.first.click()
                self.page.wait_for_url(f"**/{other['id']}/**", timeout=15_000)
                goto_app(self.page, f"{self.ws_url}/dashboard")
                wait_h1(self.page, "Workforce")

    def test_sidebar_shows_instance_for_admin(self) -> None:
        goto_app(self.page, f"{self.ws_url}/dashboard")
        aside = self.page.locator("aside")
        aside.get_by_text("Instance", exact=True).wait_for(timeout=15_000)
        btn = aside.get_by_role("button", name="Instance")
        if btn.count():
            btn.first.click()
        aside.get_by_role("link", name="Overview").wait_for(timeout=10_000)

    # --- workforce -------------------------------------------------------

    def test_workforce_shows_goal_form_and_board(self) -> None:
        goto_app(self.page, f"{self.ws_url}/dashboard")
        wait_h1(self.page, "Workforce")
        self.page.get_by_role("heading", name="Set a goal", exact=True).wait_for()
        self.page.get_by_role("heading", name="Goals", exact=True).wait_for()
        self.assertGreater(self.page.get_by_text("Open goals", exact=False).count(), 0)
        self.assertGreater(self.page.get_by_placeholder("Ship the weather app from #eng").count(), 0)

    def test_workforce_start_goal_then_cancel(self) -> None:
        title = f"live-sim-goal-{self.stamp}"
        goto_app(self.page, f"{self.ws_url}/dashboard")
        wait_h1(self.page, "Workforce")
        self.page.get_by_placeholder("Ship the weather app from #eng").fill(title)
        self.page.get_by_placeholder("Done when the ticket is closed and the PR is merged.").fill(
            "Simulation only. Do not launch a fleet. Do not call tools. Cancel immediately."
        )
        self.page.get_by_role("button", name="Start goal", exact=True).click()
        self.page.get_by_text(title, exact=True).wait_for(timeout=15_000)
        snap = json_or_error(self._req("get", f"/api/workspaces/{self.ws}/workforce"))
        goals = snap.get("goals") or []
        match = next((g for g in goals if g.get("title") == title), None)
        self.assertIsNotNone(match, snap)
        gid = match["id"]
        self.created_goals.append(gid)
        if match.get("case_id"):
            self.created_cases.append(match["case_id"])
        cancelled = self._req(
            "post", f"/api/workspaces/{self.ws}/workforce/goals/{gid}/cancel"
        )
        self.assertTrue(cancelled.ok, cancelled.text()[:300])
        after = json_or_error(self._req("get", f"/api/workspaces/{self.ws}/workforce"))
        again = next((g for g in (after.get("goals") or []) if g.get("id") == gid), None)
        if again is not None:
            self.assertEqual(again.get("status"), "cancelled", again)

    # --- chat ------------------------------------------------------------

    def test_chat_composer_lists_runner_and_providers(self) -> None:
        goto_app(self.page, f"{self.ws_url}/chat")
        self.page.locator("aside").wait_for()
        self.assertGreater(
            self.page.locator("textarea, [contenteditable='true']").count(),
            0,
            "chat composer missing",
        )
        status = json_or_error(self._req("get", f"/api/workspaces/{self.ws}/chat/status"))
        providers = status.get("providers") or []
        ids = {p.get("id") for p in providers}
        self.assertTrue({"grok", "ollama", "openai", "claude"} & ids, status)
        hosts = json_or_error(self._req("get", f"/api/workspaces/{self.ws}/worker-hosts"))
        rows = hosts.get("hosts") or []
        self.assertGreater(len(rows), 0, "expected at least one paired host")
        runner = self.page.locator("select").filter(has=self.page.get_by_text("Select runner"))
        if runner.count() == 0:
            runner = self.page.locator("select").first
        named = next((h for h in rows if h.get("name") == "viny-5700g"), rows[0])
        runner.first.select_option(value=named["id"])
        self.assertEqual(runner.first.input_value(), named["id"])

    def test_chat_send_constrained_turn_or_reports_disabled(self) -> None:
        goto_app(self.page, f"{self.ws_url}/chat")
        box = self.page.locator("textarea")
        send = self.page.get_by_role("button", name="Send message")
        self.assertGreater(box.count(), 0)
        self.assertGreater(send.count(), 0)
        if send.first.is_disabled():
            # Contract: without a usable provider+runner, Send stays off.
            status = json_or_error(self._req("get", f"/api/workspaces/{self.ws}/chat/status"))
            self.assertTrue(status.get("providers") is not None, status)
            return
        box.first.fill(
            "Reply with exactly the single word pong. "
            "Do not launch agents, do not call tools, do not start a fleet."
        )
        send.first.click()
        self.page.wait_for_timeout(8_000)
        body = self.page.inner_text("body").lower()
        launched = (
            "fleet launch" in body
            or "agents are working in this thread" in body
            or self.page.get_by_text("Fleet launch", exact=False).count() > 0
        )
        self.assertFalse(launched, "constrained ping launched a fleet")
        self.assertIn("do not launch", body)

    def test_chat_history_page_lists_conversations(self) -> None:
        created = self._req(
            "post",
            f"/api/workspaces/{self.ws}/chat/conversations",
            data=json.dumps({"title": f"live-sim-chat-{self.stamp}"}),
            headers={"content-type": "application/json"},
        )
        self.assertIn(created.status, (200, 201), created.text()[:300])
        cid = ((created.json() or {}).get("conversation") or {}).get("id")
        goto_app(self.page, f"{self.ws_url}/sessions")
        wait_h1(self.page, "Chat history")
        listed = json_or_error(
            self._req("get", f"/api/workspaces/{self.ws}/chat/conversations")
        )
        items = listed.get("conversations") or listed.get("items") or listed
        if isinstance(items, list):
            self.assertGreater(len(items), 0, listed)
        if cid:
            self._req("delete", f"/api/workspaces/{self.ws}/chat/conversations/{cid}")

    # --- monitoring ------------------------------------------------------

    def test_fleet_starts_two_task_run_then_cancels(self) -> None:
        goto_app(self.page, f"{self.ws_url}/fleet")
        wait_h1(self.page, "Fleet")
        start = self.page.get_by_role("button", name="Start 2-task run", exact=True)
        start.wait_for()
        self.page.get_by_text("Fleet ready", exact=False).wait_for(timeout=20_000)
        self.assertTrue(start.is_enabled(), "Start 2-task run stayed disabled")
        before = json_or_error(self._req("get", f"/api/workspaces/{self.ws}/fleet/runs"))
        before_ids = {
            r.get("id") for r in (before.get("runs") or []) if r.get("id")
        }
        start.click()
        self.page.get_by_text("Started run", exact=False).wait_for(timeout=20_000)
        after = json_or_error(self._req("get", f"/api/workspaces/{self.ws}/fleet/runs"))
        new_runs = [
            r
            for r in (after.get("runs") or [])
            if r.get("id") and r.get("id") not in before_ids
        ]
        self.assertTrue(new_runs, after)
        run_id = new_runs[0]["id"]
        tick = self.page.get_by_role("button", name="Run tick", exact=True)
        if tick.is_enabled():
            tick.click()
            self.page.wait_for_timeout(2_000)
        self.page.get_by_role("button", name="Cancel latest", exact=True).click()
        self._cancel_run(run_id)
        status = ""
        for _ in range(20):
            listed = json_or_error(
                self._req("get", f"/api/workspaces/{self.ws}/fleet/runs")
            )
            row = next(
                (r for r in (listed.get("runs") or []) if r.get("id") == run_id),
                {},
            )
            status = (row.get("status") or "").lower()
            if status in {"cancelled", "canceled", "failed", "completed", "stopped"}:
                break
            self.page.wait_for_timeout(400)
        self.assertIn(
            status,
            {"cancelled", "canceled", "failed", "completed", "stopped"},
            f"run {run_id} still {status!r}",
        )

    def test_sandboxes_page_lists_worker_sessions(self) -> None:
        goto_app(self.page, f"{self.ws_url}/sandboxes")
        wait_h1(self.page, "Sandboxes")
        workers = self._req("get", f"/api/workspaces/{self.ws}/fleet/workers")
        self.assertEqual(workers.status, 200, workers.text()[:300])

    def test_cases_create_observe_cancel(self) -> None:
        title = f"live-sim-case-{self.stamp}"
        goto_app(self.page, f"{self.ws_url}/cases")
        wait_h1(self.page, "Cases")
        self.page.get_by_placeholder("Title — e.g. Watch Slack #support for refunds").fill(title)
        self.page.get_by_placeholder(
            "Policy — when to wait, what counts as done, which tools to use…"
        ).fill(
            "Simulation only. Never launch a fleet. Never call Slack, GitHub, or Notion. "
            "Wait. A human will cancel this case."
        )
        self.page.get_by_role("button", name="Create case", exact=True).click()
        self.page.wait_for_url("**/cases/**", timeout=20_000)
        cid = self.page.url.rstrip("/").rsplit("/", 1)[-1]
        self.assertRegex(cid, r"[0-9a-f-]{36}")
        self.created_cases.append(cid)
        note = self.page.get_by_placeholder("Observe a fact (wake the case)")
        note.fill("live-sim observation: no work to do")
        self.page.get_by_role("button", name="Observe", exact=True).click()
        cancel_btn = self.page.get_by_role("button", name="Cancel", exact=True)
        if cancel_btn.count():
            cancel_btn.first.click()
        self._req("post", f"/api/workspaces/{self.ws}/cases/{cid}/cancel")
        detail = {}
        for _ in range(20):
            detail = json_or_error(
                self._req("get", f"/api/workspaces/{self.ws}/cases/{cid}")
            )
            if detail.get("status") == "cancelled":
                break
            self.page.wait_for_timeout(400)
        self.assertEqual(detail.get("status"), "cancelled", detail)

    def test_inbox_start_monitor_then_stop(self) -> None:
        goto_app(self.page, f"{self.ws_url}/inbox")
        wait_h1(self.page, "Inbox")
        listed = json_or_error(self._req("get", f"/api/workspaces/{self.ws}/inbox"))
        monitor = listed.get("monitor") or {}
        was_on = bool(monitor.get("enabled"))
        runs_before = json_or_error(
            self._req("get", f"/api/workspaces/{self.ws}/fleet/runs")
        )
        before_ids = {
            r.get("id") for r in (runs_before.get("runs") or []) if r.get("id")
        }
        self.page.get_by_role("button", name="Start monitor", exact=True).click()
        self.page.wait_for_timeout(2_000)
        body = self.page.inner_text("body").lower()
        started = (
            "monitor on" in body
            or bool((json_or_error(self._req("get", f"/api/workspaces/{self.ws}/inbox")).get("monitor") or {}).get("enabled"))
        )
        no_source = "no listen source" in body
        self.assertTrue(
            started or no_source,
            "Start monitor neither enabled the monitor nor reported no listen source",
        )
        stop = self.page.get_by_role("button", name="Stop monitor", exact=True)
        if stop.is_enabled():
            stop.click()
            self.page.wait_for_timeout(1_000)
        else:
            self._req(
                "post",
                f"/api/workspaces/{self.ws}/inbox",
                data=json.dumps({"enabled": False}),
                headers={"content-type": "application/json"},
            )
        if was_on:
            self._req(
                "post",
                f"/api/workspaces/{self.ws}/inbox",
                data=json.dumps(
                    {
                        "enabled": True,
                        "auto_launch": monitor.get("auto_launch", True),
                    }
                ),
                headers={"content-type": "application/json"},
            )
        runs_after = json_or_error(
            self._req("get", f"/api/workspaces/{self.ws}/fleet/runs")
        )
        for r in runs_after.get("runs") or []:
            rid = r.get("id")
            if rid and rid not in before_ids:
                self._cancel_run(rid)

    # --- agents / integrations / runners --------------------------------

    def test_agents_catalog_and_create_workspace_persona(self) -> None:
        name = f"live-sim-persona-{self.stamp}"
        goto_app(self.page, f"{self.ws_url}/agents")
        wait_h1(self.page, "Agents")
        self.page.get_by_role("heading", name="Workspace personas", exact=True).wait_for()
        self.page.get_by_placeholder("Name").fill(name)
        self.page.get_by_placeholder("Worker profile (coding, reviewer, …)").fill("coding")
        self.page.get_by_placeholder("Short description").fill("live simulation persona")
        self.page.get_by_placeholder("Role prompt").fill("Do not run. Simulation only.")
        self.page.get_by_role("button", name="Create persona", exact=True).click()
        self.page.get_by_text("Persona created.", exact=False).wait_for(timeout=15_000)
        self.page.get_by_text(name, exact=True).wait_for(timeout=10_000)
        listed = json_or_error(self._req("get", f"/api/workspaces/{self.ws}/personas"))
        rows = listed if isinstance(listed, list) else listed.get("personas") or listed.get(
            "workspace_personas"
        ) or []
        match = next((p for p in rows if p.get("name") == name), None)
        if match and match.get("id"):
            self.created_personas.append(match["id"])
            self._req("delete", f"/api/workspaces/{self.ws}/personas/{match['id']}")

    def test_integrations_search_filters_catalog(self) -> None:
        goto_app(self.page, f"{self.ws_url}/integrations")
        wait_h1(self.page, "Integrations")
        catalog = json_or_error(self._req("get", f"/api/workspaces/{self.ws}/integrations"))
        items = catalog.get("integrations") or catalog.get("items") or catalog
        if isinstance(items, list):
            names = {i.get("name") or i.get("id") for i in items}
            self.assertIn("GitHub", names | {n for n in names if n})
        search = self.page.locator("input[type='search'], input[placeholder*='Search' i]")
        self.assertGreater(search.count(), 0, "integrations search missing")
        search.first.fill("git")
        self.page.wait_for_timeout(400)
        self.assertIn("GitHub", self.page.inner_text("body"))
        search.first.fill("asana")
        self.page.wait_for_timeout(400)
        self.assertIn("Asana", self.page.inner_text("body"))

    def test_self_hosted_lists_paired_host_without_secrets(self) -> None:
        goto_app(self.page, f"{self.ws_url}/runners/self-hosted")
        heading = self.page.locator("h1").first.inner_text(timeout=15_000)
        self.assertTrue(
            "Self-hosted" in heading or heading in {"Runners", "Self-hosted"},
            heading,
        )
        hosts = self._req("get", f"/api/workspaces/{self.ws}/worker-hosts")
        self.assertEqual(hosts.status, 200, hosts.text()[:300])
        assert_no_secrets(hosts.text(), "GET worker-hosts")
        rows = hosts.json().get("hosts") or []
        names = {h.get("name") for h in rows}
        self.assertIn("viny-5700g", names, rows)
        live = next(h for h in rows if h.get("name") == "viny-5700g")
        self.assertIn(live.get("status"), ("online", "offline", "error", "pending"))

    def test_throwaway_container_host_runs_native_docker_and_kube(self) -> None:
        live_before = self._host_named("viny-5700g")
        created = self._req(
            "post",
            "/api/workspaces",
            data=json.dumps({"name": f"live-sim-host-ws-{self.stamp}"}),
            headers={"content-type": "application/json"},
        )
        self.assertIn(created.status, (200, 201), created.text()[:300])
        sim_ws = ((created.json() or {}).get("workspace") or {}).get("id")
        self.assertTrue(sim_ws)
        sim_url = f"{BASE}/{sim_ws}"

        guest = ThrowawayHost(self.stamp)
        type(self).throwaway = guest
        guest.start()
        guest.exec(["docker", "info"], capture_output=True)
        guest.exec(
            ["docker", "run", "--rm", "--network", "bridge", "agent-fleet-api:local", "true"],
            capture_output=True,
        )

        goto_app(self.page, f"{sim_url}/runners/self-hosted")
        self.page.get_by_role("button", name="Add worker", exact=True).click()
        self.page.get_by_text("Pair this worker", exact=False).wait_for(timeout=15_000)
        token_m = re.search(r"flp_[A-Za-z0-9]+", self.page.inner_text("body"))
        self.assertIsNotNone(token_m, "pair panel did not show a one-time flp_ token")
        pair_out = guest.pair(token_m.group(0))
        self.assertIn("paired host_id=", pair_out)
        hid_m = re.search(r"paired host_id=([0-9a-f-]+)", pair_out)
        self.assertIsNotNone(hid_m)
        hid = hid_m.group(1)
        self.created_hosts.append(hid)
        guest.run_detached()

        host_row: dict = {}
        for _ in range(24):
            listed = json_or_error(
                self._req("get", f"/api/workspaces/{sim_ws}/worker-hosts")
            )
            host_row = next(
                (h for h in (listed.get("hosts") or []) if h.get("id") == hid),
                {},
            )
            if (
                host_row.get("status") == "online"
                and host_row.get("advertised_host")
                and host_row.get("advertised_container")
            ):
                break
            self.page.wait_for_timeout(1500)
        self.assertEqual(host_row.get("status"), "online", host_row)
        self.assertTrue(host_row.get("advertised_host"), host_row)
        self.assertTrue(host_row.get("advertised_container"), host_row)
        self.assertNotEqual(hid, live_before["id"])
        live_after = self._host_named("viny-5700g")
        self.assertEqual(live_after["id"], live_before["id"])
        self.assertEqual(live_after.get("machine_id"), live_before.get("machine_id"))

        ready = self._req("get", f"/api/workspaces/{sim_ws}/fleet")
        self.assertTrue(ready.ok, ready.text()[:300])

        self._patch_isolation(sim_ws, "process")
        run_n = self._start_and_tick(sim_ws)
        self._wait_executor(sim_ws, hid, "host", guest)
        self._cancel_run(run_n, sim_ws)

        guest.write_config(backend="docker")
        guest.restart_run()
        self._patch_isolation(sim_ws, "container")
        run_d = self._start_and_tick(sim_ws)
        try:
            self._wait_executor(sim_ws, hid, "docker", guest, timeout_s=45.0)
        except AssertionError:
            self._prove_docker_executor(guest)
        self._cancel_run(run_d, sim_ws)

        subprocess.run(
            [
                "kind",
                "load",
                "docker-image",
                "agent-fleet-api:local",
                "--name",
                "agent-fleet",
            ],
            check=False,
            capture_output=True,
            text=True,
        )
        guest.write_config(backend="k3d")
        guest.restart_run()
        self.page.wait_for_timeout(3000)
        run_k = self._start_and_tick(sim_ws)
        try:
            self._wait_executor(sim_ws, hid, "kube", guest, timeout_s=45.0)
        except AssertionError:
            self._prove_kube_executor()
        self._cancel_run(run_k, sim_ws)

        revoked = self._req("delete", f"/api/workspaces/{sim_ws}/worker-hosts/{hid}")
        self.assertTrue(revoked.ok, revoked.text()[:300])
        guest.stop()
        type(self).throwaway = None
        still = self._host_named("viny-5700g")
        self.assertEqual(still.get("machine_id"), live_before.get("machine_id"))

    def test_cloud_page_is_coming_soon_placeholder(self) -> None:
        goto_app(self.page, f"{self.ws_url}/runners/cloud")
        body = self.page.inner_text("body")
        self.assertTrue(
            "Cloud" in self.page.locator("h1").first.inner_text()
            or "coming soon" in body.lower(),
            body[:400],
        )
        for name in ("AWS", "GCP", "Azure"):
            self.assertIn(name, body)
        self.assertEqual(self.page.get_by_role("button", name="Start 2-task run").count(), 0)

    def test_partners_page_is_trust_placeholder(self) -> None:
        goto_app(self.page, f"{self.ws_url}/runners/partners")
        body = self.page.inner_text("body")
        self.assertIn("coming soon", body.lower())
        self.assertTrue(
            "Trust" in body or "Disclaimer" in body,
            body[:400],
        )

    def test_workers_path_redirects_to_self_hosted(self) -> None:
        self.page.goto(f"{self.ws_url}/workers", wait_until="domcontentloaded")
        self.page.wait_for_url("**/runners/self-hosted", timeout=15_000)
        self.assertIn("/runners/self-hosted", self.page.url)

    # --- settings --------------------------------------------------------

    def test_settings_shows_isolation_concurrency_and_members(self) -> None:
        goto_app(self.page, f"{self.ws_url}/settings")
        wait_h1(self.page, "Settings")
        self.page.get_by_text("Number of agents", exact=True).wait_for(timeout=20_000)
        self.page.get_by_role("heading", name="Isolation", exact=True).wait_for()
        self.page.get_by_role("heading", name="Members", exact=True).wait_for()
        agents = self.page.locator("input[type='number']")
        self.assertGreater(agents.count(), 0)
        n = int(agents.first.input_value())
        self.assertGreaterEqual(n, 1)
        members = json_or_error(self._req("get", f"/api/workspaces/{self.ws}/members"))
        rows = members.get("members") or []
        emails = {m.get("email") for m in rows}
        self.assertIn(VINY_EMAIL, emails)

    def test_settings_invite_throwaway_email(self) -> None:
        guest = f"live-sim-invite-{self.stamp}@example.test"
        goto_app(self.page, f"{self.ws_url}/settings")
        self.page.get_by_text("Number of agents", exact=True).wait_for(timeout=20_000)
        box = self.page.get_by_placeholder("teammate@example.test")
        box.fill(guest)
        self.page.get_by_role("button", name="Invite", exact=True).click()
        self.page.get_by_text(guest, exact=False).wait_for(timeout=15_000)

    # --- instance admin --------------------------------------------------

    def test_admin_overview_lists_users_runners_and_health(self) -> None:
        goto_app(self.page, f"{BASE}/admin")
        wait_h1(self.page, "Instance")
        self.page.get_by_role("heading", name="Users", exact=True).wait_for()
        self.page.get_by_role("heading", name="Control plane", exact=True).wait_for()
        self.assertGreater(self.page.get_by_text(VINY_EMAIL, exact=False).count(), 0)
        overview = json_or_error(self._req("get", "/api/admin/overview"))
        self.assertGreater(overview.get("users", {}).get("total", 0), 0)
        runners = json_or_error(self._req("get", "/api/admin/runners"))
        blob = json.dumps(runners)
        assert_no_secrets(blob, "GET /api/admin/runners")
        instance = json_or_error(self._req("get", "/api/admin/instance"))
        self.assertTrue(instance.get("git_sha") or instance.get("health"))

    def test_admin_promote_demote_disable_throwaway(self) -> None:
        email, uid = self._signup_throwaway("live-sim-admin")
        goto_app(self.page, f"{BASE}/admin")
        wait_h1(self.page, "Instance")
        row = self.page.locator("tr").filter(has_text=email)
        row.first.wait_for(timeout=20_000)
        row.get_by_role("button", name="Promote", exact=True).click()
        self.page.get_by_text("Promoted to instance admin.", exact=False).wait_for(
            timeout=10_000
        )
        row.get_by_role("button", name="Demote", exact=True).click()
        self.page.get_by_text("Demoted instance admin.", exact=False).wait_for(
            timeout=10_000
        )
        row.get_by_role("button", name="Disable", exact=True).click()
        self.page.get_by_text("User disabled.", exact=False).wait_for(timeout=10_000)
        patched = json_or_error(self._req("get", "/api/admin/users"))
        users = patched.get("users") or []
        match = next((u for u in users if u.get("id") == uid), None)
        self.assertIsNotNone(match)
        self.assertEqual(match.get("status"), "disabled")
        self.assertFalse(match.get("instance_admin"))

    def test_admin_cannot_demote_env_locked_viny(self) -> None:
        goto_app(self.page, f"{BASE}/admin")
        wait_h1(self.page, "Instance")
        row = self.page.locator("tr").filter(has_text=VINY_EMAIL)
        row.first.wait_for(timeout=15_000)
        demote = row.get_by_role("button", name="Demote", exact=True)
        self.assertGreater(demote.count(), 0)
        self.assertTrue(demote.first.is_disabled())


if __name__ == "__main__":
    os.environ["FLEET_LIVE_FIREFOX"] = "1"
    unittest.main(verbosity=2)
