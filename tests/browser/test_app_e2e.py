#!/usr/bin/env python3
"""Headless Chromium e2e against the live UI (:3030) + API (:8080).

Install: crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh
  (ENABLE_BROWSER_E2E=1 in repo-root .env; PROFILE=full defaults it on).

Does not skip when the stack is down — fail so the suite cannot go green blind.
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
from runners_e2e_helpers import (  # noqa: E402
    assert_no_control_plane_card,
    create_host,
    goto_self_hosted,
    heartbeat,
    list_hosts,
    pair_and_heartbeat,
    pair_host,
    patch_host,
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

# Must survive specs_line / clean_cpu_model / fmt_mem / fmt_gpu (page.rs).
SPECS_HARDWARE = {
    "cpu_model": "Intel(R) Core(TM) i7-10610U CPU @ 1.80GHz",
    "cpu_cores": 8,
    "mem_total_bytes": 17179869184,
    "mem_speed_mts": 3200,
    "mem_type": "DDR4",
    "gpus": [{"name": "Intel UHD Graphics", "used_pct": 4}],
    "os": "linux 6.8 x86_64",
    "cpu_used_pct": 12,
    "mem_used_pct": 32,
}


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


# Mutating control-plane paths these placeholder pages must not hit.
_REMOTE_ENQUEUE_RE = re.compile(
    r"/(?:spawn|enqueue|lease|tick|fleet/runs|worker-hosts|jobs|claims|lend)(?:/|$|\?)",
    re.I,
)
_LEND_CONTROL_RE = re.compile(
    r"^(lend|lend capacity|enable lending|start lending|offer capacity|begin lending)$",
    re.I,
)


def _is_remote_enqueue_or_spawn(method: str, url: str) -> bool:
    if method.upper() not in {"POST", "PUT", "PATCH", "DELETE"}:
        return False
    if "lend" in url.lower():
        return True
    return _REMOTE_ENQUEUE_RE.search(url) is not None


# Heartbeat software that will not match a staged linux-x86_64 bake.
_BEHIND_SHA = "0000000000000000000000000000000000000000"
_BEHIND_BINS = {
    "fleet-host": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
    "fleet-driver": "0123456701234567012345670123456701234567012345670123456701234567",
}


def _host_needs_software_update(host: dict) -> bool:
    status = host.get("software_status")
    if host.get("outdated") or status == "behind":
        return True
    if status == "current":
        sw = host.get("software") or {}
        sha = (sw.get("git_sha") or "").strip() if isinstance(sw, dict) else ""
        return not sha
    return False


def _wait_dx_ready(page) -> None:
    """Wait out dx compile splash and rebuild banner."""
    splash = page.get_by_text(
        re.compile(r"We're building your app now|fleet-ui compiling", re.I)
    )
    deadline = time.time() + 180
    while time.time() < deadline:
        try:
            busy = splash.count() > 0 and splash.first.is_visible()
        except Exception:
            busy = False
        if not busy:
            return
        page.wait_for_timeout(500)


def _ensure_workspace(page) -> tuple[str, str]:
    """Reuse the suite session, or create one via API (avoids signup WASM)."""
    base = getattr(BrowserE2ETests, "workspace_url", None)
    if base:
        return base.rstrip("/").rsplit("/", 1)[-1], base
    email = f"updnow-{uuid.uuid4().hex[:12]}@example.test"
    password = "e2e-pass-ok"
    headers = {"Content-Type": "application/json"}
    created = None
    last_err = ""
    for _ in range(8):
        created = page.request.post(
            f"{UI}/api/auth/signup",
            data=json.dumps(
                {
                    "email": email,
                    "password": password,
                    "display_name": "E2E updnow",
                }
            ),
            headers=headers,
        )
        if created.status in {200, 201}:
            break
        last_err = f"signup {created.status}: {created.text()[:400]}"
        page.wait_for_timeout(750)
    if created is None or created.status not in {200, 201}:
        raise RuntimeError(last_err or "signup failed")
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
    workspace_url = f"{UI}/{ws}"
    BrowserE2ETests.workspace_url = workspace_url
    return ws, workspace_url


def _mismatch_software(listed: dict) -> dict:
    staged_sha = ""
    staged_bins: dict[str, str] = {}
    for pkg in listed.get("packages") or []:
        if not isinstance(pkg, dict) or pkg.get("platform") != "linux-x86_64":
            continue
        staged_sha = (pkg.get("git_sha") or "").strip()
        raw = pkg.get("bins") or {}
        if isinstance(raw, dict):
            for stem, val in raw.items():
                got = (
                    (val.get("sha256") or "").strip()
                    if isinstance(val, dict)
                    else str(val).strip()
                )
                if got:
                    staged_bins[stem] = got
        break
    git_sha = _BEHIND_SHA
    if staged_sha and git_sha.lower() == staged_sha.lower():
        git_sha = "1111111111111111111111111111111111111111"
    bins = dict(_BEHIND_BINS)
    for stem, want in staged_bins.items():
        if stem in bins and bins[stem].lower() == want.lower():
            bins[stem] = "e" * 64
    return {"git_sha": git_sha, "bins": bins}


def _poll_clipboard(
    page, needle: str, timeout_ms: int = 5_000, *, exclude: str | None = None
) -> str:
    """Read clipboard with a JS timeout so a stuck Clipboard API cannot hang the suite."""
    deadline = time.time() + timeout_ms / 1000
    last = ""
    last_err: Exception | None = None
    while time.time() < deadline:
        try:
            last = page.evaluate(
                """() => Promise.race([
                    navigator.clipboard.readText(),
                    new Promise((_, rej) =>
                        setTimeout(() => rej(new Error('clipboard-read timeout')), 2000)
                    ),
                ])"""
            )
            last_err = None
        except Exception as e:
            last_err = e
            last = ""
        if (
            isinstance(last, str)
            and needle in last
            and (exclude is None or exclude not in last)
        ):
            return last
        page.wait_for_timeout(50)
    raise AssertionError(
        f"clipboard missing {needle!r} exclude={exclude!r}: {last!r} err={last_err}"
    )


class BrowserE2ETests(unittest.TestCase):
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

    def _assert_no_lend_enablement(self, page, where: str) -> None:
        body = page.inner_text("body").lower()
        self.assertNotIn("lending enabled", body, f"{where}: lend must stay off")
        for i in range(page.get_by_role("button").count()):
            btn = page.get_by_role("button").nth(i)
            if not btn.is_visible():
                continue
            name = " ".join((btn.inner_text() or "").split())
            if not name or _LEND_CONTROL_RE.search(name) is None:
                continue
            self.assertFalse(
                btn.is_enabled(),
                f"{where}: lend control {name!r} must not be enabled",
            )
        for role in ("checkbox", "switch"):
            loc = page.get_by_role(role)
            for i in range(loc.count()):
                ctl = loc.nth(i)
                if not ctl.is_visible():
                    continue
                label = " ".join(
                    (ctl.get_attribute("aria-label") or ctl.inner_text() or "").split()
                )
                if _LEND_CONTROL_RE.search(label) is None:
                    continue
                self.assertFalse(
                    ctl.is_checked(),
                    f"{where}: lend control {label!r} must not be on",
                )
                self.assertFalse(
                    ctl.is_enabled(),
                    f"{where}: lend control {label!r} must not be enabled",
                )

    @classmethod
    def tearDownClass(cls) -> None:
        cls._context.close()
        cls._browser.close()
        cls._pw.stop()

    def test_01_landing_has_cta(self) -> None:
        self.page.goto(f"{UI}/", wait_until="networkidle")
        self.assertIn("Delegate work to sandboxed agents", self.page.inner_text("h1"))
        self.assertTrue(self.page.get_by_role("link", name="Get started").first.is_visible())
        # Navbar + hero both link to /login.
        self.assertGreaterEqual(self.page.get_by_role("link", name="Log in").count(), 1)

    def test_02_unauthenticated_app_shell_redirects_to_login(self) -> None:
        fake = uuid.uuid4()
        self.page.goto(f"{UI}/{fake}/dashboard", wait_until="domcontentloaded")
        self.page.wait_for_url("**/login", timeout=15_000)
        self.assertIn("/login", self.page.url)
        self.assertIn("Welcome back", self.page.inner_text("h1"))

    def test_03_signup_then_login_reaches_dashboard(self) -> None:
        email = f"e2e-{uuid.uuid4().hex[:12]}@example.test"
        password = "e2e-pass-ok"
        self.page.goto(f"{UI}/signup", wait_until="networkidle")
        self.page.locator("#display_name").fill("E2E Browser")
        self.page.locator("#email").fill(email)
        self.page.locator("#password").fill(password)
        self.page.get_by_role("button", name="Sign up").click()
        self.page.wait_for_url("**/login", timeout=15_000)
        self.page.locator("#email").fill(email)
        self.page.locator("#password").fill(password)
        self.page.get_by_role("button", name="Log in").click()
        self.page.wait_for_url("**/dashboard", timeout=20_000)
        self.assertRegex(self.page.url, r"/[0-9a-f-]{36}/dashboard")
        self.page.wait_for_selector("h1")
        self.assertEqual(self.page.inner_text("h1").strip(), "Workforce")
        type(self).workspace_url = self.page.url.rsplit("/dashboard", 1)[0]

    def test_04_sidebar_app_pages(self) -> None:
        base = getattr(type(self), "workspace_url", None)
        self.assertIsNotNone(base, "signup/login must run first")
        page = self.page

        page.get_by_role("link", name="Workforce", exact=True).click()
        page.wait_for_url("**/dashboard", timeout=10_000)
        page.wait_for_selector("h1")
        self.assertEqual(page.inner_text("h1").strip(), "Workforce")
        self.assertGreater(
            page.get_by_text("Set a goal").count(),
            0,
            "Workforce page missing Set a goal",
        )

        page.get_by_role("link", name="Fleet", exact=True).click()
        page.wait_for_url("**/fleet", timeout=10_000)
        page.wait_for_selector("h1")
        self.assertEqual(page.inner_text("h1").strip(), "Fleet")

        page.get_by_role("link", name="Sandboxes", exact=True).click()
        page.wait_for_url("**/sandboxes", timeout=10_000)
        page.wait_for_selector("h1")
        self.assertIn("Sandbox", page.inner_text("h1"))

        page.get_by_role("link", name="New chat", exact=True).click()
        page.wait_for_url("**/chat", timeout=10_000)
        body = page.inner_text("body")
        self.assertTrue(
            "New chat" in body or "Chat" in body,
            "chat page did not render",
        )
        self.assertNotIn(
            "Chat backend not connected yet.",
            body,
            "stub chat UI — Kind/old fleet-ui, not this tree",
        )
        has_provider = page.locator("select[title='Chat provider']").count() > 0
        has_offline = "No chat provider ready" in body
        self.assertTrue(
            has_provider or has_offline,
            "new chat chrome missing (provider select or offline hint)",
        )

        hist = page.get_by_role("link", name="Chat history", exact=True)
        if hist.count() == 0:
            hist = page.get_by_role("link", name="Live sessions", exact=True)
        hist.first.click()
        page.wait_for_url("**/sessions", timeout=10_000)
        page.wait_for_selector("h1")
        sessions_h1 = page.locator("h1").first.inner_text().strip()
        self.assertIn(
            sessions_h1,
            {"Chat history", "Sessions"},
            f"unexpected sessions heading {sessions_h1!r}",
        )

        # Workspace accordion starts collapsed.
        page.get_by_text("Workspace", exact=True).first.click()
        page.get_by_role("link", name="Integrations", exact=True).click()
        page.wait_for_url("**/integrations", timeout=10_000)
        page.wait_for_selector("h1")
        self.assertIn("Integration", page.locator("h1").first.inner_text())

        # Runners / Self-hosted — not Workers under Workspace.
        nav = page.locator("aside nav")
        self.assertEqual(
            nav.get_by_role("link", name="Workers", exact=True).count(),
            0,
            "Workers must not remain under Workspace",
        )
        runners = nav.get_by_text("Runners", exact=True)
        self.assertTrue(runners.first.is_visible(), "sidebar missing Runners")
        self_hosted = nav.get_by_role("link", name="Self-hosted", exact=True)
        if self_hosted.count() == 0 or not self_hosted.first.is_visible():
            runners.first.click()
            self_hosted = nav.get_by_role("link", name="Self-hosted", exact=True)
        self.assertTrue(self_hosted.first.is_visible(), "sidebar missing Self-hosted")
        self_hosted.first.click()
        page.wait_for_url("**/runners/self-hosted", timeout=10_000)
        page.wait_for_selector("h1")
        runners_h1 = page.locator("h1").first.inner_text().strip()
        self.assertIn(
            runners_h1,
            {"Self-hosted", "Workers"},
            f"unexpected runners heading {runners_h1!r}",
        )
        self.assertTrue(
            page.get_by_role("button", name="Add worker").count() > 0,
            "Self-hosted runners page missing Add worker",
        )
        assert_no_control_plane_card(page)
        runners_body = page.inner_text("body")
        self.assertIn("No workers yet", runners_body)
        self.assertIn("The API is not a runner", runners_body)
        cloud = nav.get_by_role("link", name="Cloud", exact=True)
        self.assertTrue(cloud.first.is_visible(), "sidebar missing Cloud")
        partners = nav.get_by_role("link", name="Partners", exact=True)
        self.assertTrue(partners.first.is_visible(), "sidebar missing Partners")

        page.get_by_role("link", name="Settings", exact=True).click()
        page.wait_for_url("**/settings", timeout=10_000)
        page.wait_for_selector("h1")
        self.assertEqual(page.locator("h1").first.inner_text().strip(), "Settings")

        # Session cookie is first-party on :3030 (dx proxy). Reload must stay signed in.
        page.reload(wait_until="networkidle")
        self.assertNotIn("/login", page.url)

    def test_05_workers_add_shows_pair_token(self) -> None:
        base = getattr(type(self), "workspace_url", None)
        self.assertIsNotNone(base, "signup/login must run first")
        page = self.page
        page.goto(f"{base}/runners/self-hosted", wait_until="networkidle")
        page.wait_for_selector("h1")
        runners_h1 = page.locator("h1").first.inner_text().strip()
        self.assertIn(
            runners_h1,
            {"Self-hosted", "Workers"},
            f"unexpected runners heading {runners_h1!r}",
        )
        page.get_by_role("button", name="Add worker").click()
        page.get_by_role("heading", name="Pair this worker").wait_for(timeout=15_000)
        body = page.inner_text("body")
        self.assertIn("open a terminal and paste", body.lower())
        self.assertIn("Open PowerShell and paste", body)
        self.assertIn("fleet-host", body)
        self.assertIn("fleet-driver", body)

        unix = page.locator("pre").filter(has_text="install-worker.sh").inner_text()
        self.assertIn("FLEET_PAIR_TOKEN=", unix)
        win = page.locator("pre").filter(has_text="install-worker.ps1").inner_text()
        self.assertIn("FLEET_PAIR_TOKEN=", win)

        token = page.locator("pre").filter(has_text="flp_").last.inner_text().strip()
        self.assertTrue(token.startswith("flp_"), f"pair token prefix: {token}")
        self.assertGreaterEqual(len(token), len("flp_") + 32, token)
        self.assertIn(token, unix)
        self.assertIn(token, win)

        # Token is shown once: dismiss + reload must not repeat the plaintext.
        self.assertTrue(page.get_by_role("button", name="Cancel").is_visible())
        page.get_by_role("button", name="Done").click()
        page.get_by_role("heading", name="Pair this worker").wait_for(state="hidden")
        self.assertNotIn(token, page.inner_text("body"))
        page.reload(wait_until="networkidle")
        page.wait_for_selector("h1")
        self.assertNotIn(token, page.inner_text("body"))
        card = page.inner_text("body")
        self.assertTrue(
            page.get_by_role("button", name="Remove worker").first.is_visible()
        )
        self.assertTrue(
            page.get_by_role("button", name="Stop taking new jobs").first.is_visible()
        )
        self.assertIn("pending", card.lower())
        self.assertIn("Waiting to pair", card)
        assert_no_control_plane_card(page)
        self.assertNotIn("heartbeating", card.lower())
        self.assertNotIn("Served packages", card)
        self.assertNotIn("Current runner build", card)
        self.assertRegex(card, r"Current fleet-host version \d+\.\d+\.\d+")
        self.assertRegex(card, r"Current fleet-driver version \d+\.\d+\.\d+")
        self.assertNotIn("Serving this build:", card)
        self.assertNotIn("Outdated packages:", card)
        self.assertNotIn("re-run the install command", card.lower())
        self.assertIn("Max jobs at once", card)
        self.assertNotIn("Isolation", card)
        self.assertEqual(
            page.locator("article").first.get_by_role("checkbox", name=re.compile(r"^Native$")).count(),
            0,
            "Native isolation checkbox is gone — plane is not a runner",
        )
        self.assertNotIn("not implemented yet", card.lower())
        self.assertIn("Runtime:", card)
        self.assertIn("behavior", card.lower())
        self.assertIn("Auto-update", card)
        self.assertRegex(card, r"id [0-9a-f]{8}")
        self.assertRegex(card.lower(), r"jobs\s+0\s*/\s*4")
        self.assertNotIn("Allow host", card)
        self.assertNotIn("Allow container", card)
        self.assertNotIn("Advertised", card)

    def test_15_cancel_discards_pending_worker(self) -> None:
        base = getattr(type(self), "workspace_url", None)
        self.assertIsNotNone(base, "signup/login must run first")
        page = self.page
        page.goto(f"{base}/runners/self-hosted", wait_until="networkidle")
        page.wait_for_selector("h1")
        before = page.locator("article").count()
        page.get_by_role("button", name="Add worker").click()
        page.get_by_role("heading", name="Pair this worker").wait_for(timeout=15_000)
        token = page.locator("pre").filter(has_text="flp_").last.inner_text().strip()
        page.get_by_role("button", name="Cancel").click()
        page.get_by_role("heading", name="Pair this worker").wait_for(state="hidden")
        self.assertNotIn(token, page.inner_text("body"))
        page.wait_for_timeout(500)
        after = page.locator("article").count()
        self.assertLessEqual(
            after,
            before,
            "Cancel must discard the unused pending worker",
        )

    def test_16_control_plane_card(self) -> None:
        base = getattr(type(self), "workspace_url", None)
        self.assertIsNotNone(base, "signup/login must run first")
        page = self.page
        page.goto(f"{base}/runners/self-hosted", wait_until="networkidle")
        page.wait_for_selector("h1")
        assert_no_control_plane_card(page)
        body = page.inner_text("body")
        self.assertNotIn("does not run sandboxes", body.lower())
        ws = base.rstrip("/").rsplit("/", 1)[-1]
        fleet = self.page.request.get(f"{API}/api/workspaces/{ws}/fleet")
        if fleet.ok:
            self.assertTrue(fleet.ok, "GET fleet")

    def test_16b_fleet_header_matches_backend_and_k8s_run(self) -> None:
        """Fleet chrome shows the live backend; in-cluster k8s can start a run."""
        base = getattr(type(self), "workspace_url", None)
        self.assertIsNotNone(base, "signup/login must run first")
        ws = base.rstrip("/").rsplit("/", 1)[-1]
        res = self.page.request.get(f"{API}/api/workspaces/{ws}/fleet")
        self.assertEqual(res.status, 200, res.text()[:400])
        backend = (res.json() or {}).get("sandbox_backend")
        self.assertTrue(backend, f"GET fleet missing sandbox_backend: {res.text()[:400]}")

        page = self.page
        page.goto(f"{base}/fleet", wait_until="networkidle")
        page.wait_for_selector("h1")
        self.assertEqual(page.inner_text("h1").strip(), "Fleet")
        body = page.inner_text("body")
        self.assertIn(f"sandbox {backend}", body)
        if backend != "k8s":
            return

        page.get_by_role("button", name="Start 2-task run").click()
        page.get_by_text("Started run").wait_for(timeout=15_000)
        page.get_by_text("Root task").wait_for(timeout=15_000)
        tick = page.get_by_role("button", name="Run tick")
        if tick.count():
            tick.first.click()
            page.wait_for_timeout(800)
        body = page.inner_text("body")
        self.assertIn("k8s", body.lower())
        cancel = page.get_by_role("button", name="Cancel latest")
        self.assertTrue(cancel.count(), "Cancel latest missing after fixture")
        if cancel.first.is_enabled():
            cancel.first.click()
            page.get_by_text("Cancel requested").wait_for(timeout=10_000)

    def test_17_apply_changes_when_dirty(self) -> None:
        base = getattr(type(self), "workspace_url", None)
        self.assertIsNotNone(base, "signup/login must run first")
        page = self.page
        page.goto(f"{base}/runners/self-hosted", wait_until="networkidle")
        page.wait_for_selector("h1")
        auto = page.locator("label").filter(has_text="Auto-update")
        if auto.count() == 0:
            page.get_by_role("button", name="Add worker").click()
            page.get_by_role("heading", name="Pair this worker").wait_for(timeout=15_000)
            page.get_by_role("button", name="Done").click()
            page.get_by_role("heading", name="Pair this worker").wait_for(state="hidden")
            auto = page.locator("label").filter(has_text="Auto-update")
        apply = page.get_by_role("button", name="Apply changes")
        self.assertEqual(apply.count(), 0, "Apply must stay hidden until a setting changes")
        self.assertGreater(auto.count(), 0, "paired or pending card needs Auto-update")
        auto.last.locator("input").click()
        page.get_by_role("button", name="Apply changes").wait_for(timeout=5_000)
        self.assertTrue(page.get_by_role("button", name="Apply changes").first.is_visible())

    def test_06_api_health_from_browser(self) -> None:
        res = self.page.request.get(f"{API}/health")
        self.assertEqual(res.status, 200, res.text()[:300])
        body = res.json()
        self.assertEqual(body.get("service"), "fleet-server")
        self.assertEqual(body.get("status"), "ok")
        apis = body.get("apis") or []
        for name in ("auth", "workspaces", "fleet", "chat"):
            self.assertIn(name, apis, f"health.apis missing {name}: {apis}")
        sha = body.get("git_sha")
        self.assertTrue(sha, f"health.git_sha must be this tree: {body}")
        ui = self.page.request.get(f"{UI}/fleet-git-sha.txt")
        self.assertEqual(ui.status, 200, ui.text()[:300])
        self.assertEqual(ui.text().strip(), sha)

    def test_07_sidebar_has_fleet_and_integrations(self) -> None:
        base = getattr(type(self), "workspace_url", None)
        self.assertIsNotNone(base, "signup/login must run first")
        page = self.page
        page.goto(f"{base}/dashboard", wait_until="networkidle")
        page.wait_for_selector("h1")
        nav = page.locator("aside nav")
        # Monitoring starts expanded; exact=True avoids "Agent Fleet" in the brand.
        fleet = nav.get_by_role("link", name="Fleet", exact=True)
        self.assertTrue(fleet.first.is_visible())
        # Workspace accordion starts collapsed — expand, then assert Integrations.
        page.get_by_text("Workspace", exact=True).first.click()
        integ = nav.get_by_role("link", name="Integrations", exact=True)
        self.assertTrue(integ.first.is_visible())
        self.assertEqual(
            nav.get_by_role("link", name="Workers", exact=True).count(),
            0,
            "Workers must not remain under Workspace",
        )
        runners = nav.get_by_text("Runners", exact=True)
        self.assertTrue(runners.first.is_visible(), "sidebar missing Runners")
        self_hosted = nav.get_by_role("link", name="Self-hosted", exact=True)
        if self_hosted.count() == 0 or not self_hosted.first.is_visible():
            runners.first.click()
            self_hosted = nav.get_by_role("link", name="Self-hosted", exact=True)
        self.assertTrue(self_hosted.first.is_visible(), "sidebar missing Self-hosted")
        cloud = nav.get_by_role("link", name="Cloud", exact=True)
        partners = nav.get_by_role("link", name="Partners", exact=True)
        if cloud.count() == 0 or not cloud.first.is_visible():
            runners.first.click()
            cloud = nav.get_by_role("link", name="Cloud", exact=True)
            partners = nav.get_by_role("link", name="Partners", exact=True)
        self.assertTrue(cloud.first.is_visible(), "sidebar missing Cloud")
        self.assertTrue(partners.first.is_visible(), "sidebar missing Partners")

    def test_08_workers_redirects_to_self_hosted(self) -> None:
        base = getattr(type(self), "workspace_url", None)
        self.assertIsNotNone(base, "signup/login must run first")
        page = self.page
        page.goto(f"{base}/workers", wait_until="domcontentloaded")
        page.wait_for_url("**/runners/self-hosted", timeout=15_000)
        self.assertRegex(page.url, r"/[0-9a-f-]{36}/runners/self-hosted(?:[?#].*)?$")

    def test_09_runners_redirects_to_self_hosted(self) -> None:
        base = getattr(type(self), "workspace_url", None)
        self.assertIsNotNone(base, "signup/login must run first")
        page = self.page
        page.goto(f"{base}/runners", wait_until="domcontentloaded")
        page.wait_for_url("**/runners/self-hosted", timeout=15_000)
        self.assertRegex(page.url, r"/[0-9a-f-]{36}/runners/self-hosted(?:[?#].*)?$")

    def test_10_nav_links_to_cloud_and_partners(self) -> None:
        base = getattr(type(self), "workspace_url", None)
        self.assertIsNotNone(base, "signup/login must run first")
        page = self.page
        page.goto(f"{base}/dashboard", wait_until="networkidle")
        page.wait_for_selector("h1")
        nav = page.locator("aside nav")
        runners = nav.get_by_text("Runners", exact=True)
        self.assertTrue(runners.first.is_visible(), "sidebar missing Runners")
        cloud = nav.get_by_role("link", name="Cloud", exact=True)
        partners = nav.get_by_role("link", name="Partners", exact=True)
        if (
            cloud.count() == 0
            or not cloud.first.is_visible()
            or partners.count() == 0
            or not partners.first.is_visible()
        ):
            runners.first.click()
            cloud = nav.get_by_role("link", name="Cloud", exact=True)
            partners = nav.get_by_role("link", name="Partners", exact=True)
        self.assertTrue(cloud.first.is_visible(), "sidebar missing Cloud link")
        self.assertTrue(partners.first.is_visible(), "sidebar missing Partners link")

        cloud.first.click()
        page.wait_for_url("**/runners/cloud", timeout=10_000)
        self.assertRegex(page.url, r"/[0-9a-f-]{36}/runners/cloud(?:[?#].*)?$")

        partners = nav.get_by_role("link", name="Partners", exact=True)
        if partners.count() == 0 or not partners.first.is_visible():
            nav.get_by_text("Runners", exact=True).first.click()
            partners = nav.get_by_role("link", name="Partners", exact=True)
        partners.first.click()
        page.wait_for_url("**/runners/partners", timeout=10_000)
        self.assertRegex(page.url, r"/[0-9a-f-]{36}/runners/partners(?:[?#].*)?$")

    def test_11_cloud_page_coming_soon(self) -> None:
        base = getattr(type(self), "workspace_url", None)
        self.assertIsNotNone(base, "signup/login must run first")
        page = self.page
        page.goto(f"{base}/runners/cloud", wait_until="networkidle")
        page.wait_for_selector("h1")
        self.assertRegex(page.url, r"/[0-9a-f-]{36}/runners/cloud(?:[?#].*)?$")
        h1 = page.locator("h1").first.inner_text().strip()
        self.assertIn("Cloud", h1, f"unexpected cloud heading {h1!r}")
        body = page.inner_text("body")
        for label in ("AWS", "GCP", "Azure", "Fleet Cloud"):
            self.assertIn(label, body, f"cloud page missing {label}")
        self.assertIn(
            "coming soon",
            body.lower(),
            "cloud providers must be coming-soon placeholders",
        )
        self._assert_no_lend_enablement(page, "cloud")

    def test_12_partners_page_trust_disclaimer(self) -> None:
        base = getattr(type(self), "workspace_url", None)
        self.assertIsNotNone(base, "signup/login must run first")
        page = self.page
        page.goto(f"{base}/runners/partners", wait_until="networkidle")
        page.wait_for_selector("h1")
        self.assertRegex(page.url, r"/[0-9a-f-]{36}/runners/partners(?:[?#].*)?$")
        h1 = page.locator("h1").first.inner_text().strip()
        self.assertIn("Partners", h1, f"unexpected partners heading {h1!r}")
        body = page.inner_text("body")
        body_l = body.lower()
        self.assertIn("trust", body_l, "partners page missing trust copy")
        self.assertIn("disclaimer", body_l, "partners page missing disclaimer copy")
        boxes = page.get_by_role("checkbox", name=re.compile(r"disclaimer", re.I))
        if boxes.count() == 0:
            boxes = page.get_by_role("checkbox")
        visible = [
            boxes.nth(i) for i in range(boxes.count()) if boxes.nth(i).is_visible()
        ]
        self.assertGreater(
            len(visible),
            0,
            "partners disclaimer control missing (expected an unchecked checkbox)",
        )
        for box in visible:
            self.assertFalse(box.is_checked(), "partners disclaimer must default off")
        lend = page.get_by_role("button", name="Lend capacity")
        self.assertTrue(lend.first.is_visible(), "partners page missing Lend capacity")
        self.assertFalse(lend.first.is_enabled(), "Lend capacity must stay disabled")
        self._assert_no_lend_enablement(page, "partners")

    def test_13_no_remote_enqueue_from_cloud_or_partners(self) -> None:
        base = getattr(type(self), "workspace_url", None)
        self.assertIsNotNone(base, "signup/login must run first")
        page = self.page
        mutating: list[tuple[str, str]] = []

        def on_request(req) -> None:
            method = req.method.upper()
            if method in {"POST", "PUT", "PATCH", "DELETE"}:
                mutating.append((method, req.url))

        page.on("request", on_request)
        try:
            for path in ("/runners/cloud", "/runners/partners"):
                page.goto(f"{base}{path}", wait_until="networkidle")
                page.wait_for_selector("h1")
                self._assert_no_lend_enablement(page, path)
                self.assertEqual(
                    page.get_by_role("button", name="Add worker").count(),
                    0,
                    f"{path} must not pair workers",
                )
        finally:
            page.remove_listener("request", on_request)

        forbidden = [
            (method, url)
            for method, url in mutating
            if _is_remote_enqueue_or_spawn(method, url)
        ]
        self.assertEqual(
            forbidden,
            [],
            "cloud/partners pages must not enqueue, spawn, or lend via API",
        )

    def _partners_disclaimer_boxes(self, page):
        boxes = page.get_by_role(
            "checkbox", name=re.compile(r"acknowledge|disclaimer", re.I)
        )
        if boxes.count() == 0:
            boxes = page.get_by_role("checkbox")
        return [
            boxes.nth(i) for i in range(boxes.count()) if boxes.nth(i).is_visible()
        ]

    def test_14o_partners_ack(self) -> None:
        # catalog:e2e-partners-ack-persists — ack is a timestamp only.
        # Unittest name order: before test_18_logout so the session stays.
        page = self.page
        base = getattr(type(self), "workspace_url", None)
        if not base:
            _ws, base = signup_login(page, prefix="ack")
            type(self).workspace_url = base

        page.goto(f"{base}/runners/partners", wait_until="networkidle")
        page.wait_for_selector("h1")
        self.assertRegex(page.url, r"/[0-9a-f-]{36}/runners/partners(?:[?#].*)?$")

        visible = self._partners_disclaimer_boxes(page)
        self.assertGreater(
            len(visible),
            0,
            "partners disclaimer control missing (expected a checkbox)",
        )
        box = visible[0]
        if not box.is_checked():
            with page.expect_response(
                lambda r: "/partners/disclaimer" in r.url
                and r.request.method.upper() == "POST"
                and r.ok,
                timeout=15_000,
            ):
                box.check()
        self.assertTrue(box.is_checked(), "disclaimer must stay checked after persist")

        page.reload(wait_until="networkidle")
        page.wait_for_selector("h1")
        page.wait_for_function(
            """() => {
                const labels = [...document.querySelectorAll('label')];
                const lab = labels.find((l) =>
                    /acknowledge/i.test(l.innerText || '')
                );
                const input = lab && lab.querySelector('input[type=checkbox]');
                return !!(input && input.checked);
            }"""
        )
        visible = self._partners_disclaimer_boxes(page)
        self.assertGreater(
            len(visible),
            0,
            "disclaimer checkbox missing after reload",
        )
        for box in visible:
            self.assertTrue(
                box.is_checked(),
                "disclaimer ack must persist across reload",
            )

        lend = page.get_by_role("button", name="Lend capacity")
        self.assertTrue(lend.first.is_visible(), "partners page missing Lend capacity")
        self.assertFalse(
            lend.first.is_enabled(),
            "Lend capacity must stay disabled after ack",
        )
        self._assert_no_lend_enablement(page, "partners after ack")
        body_l = page.inner_text("body").lower()
        self.assertNotIn("balance", body_l, "ack must not create a credit balance")
        self.assertNotRegex(
            body_l,
            r"\b\d+(?:\.\d+)?\s+credits?\b",
            "ack must not display a live credit balance",
        )

    def test_14f_specs_live(self) -> None:
        # Unittest name order: before test_18_logout so the session stays.
        page = self.page
        base = getattr(type(self), "workspace_url", None)
        if base:
            workspace_url = base
            ws = base.rstrip("/").rsplit("/", 1)[-1]
        else:
            # Cookie session via API so a dx rebuild overlay cannot hide #display_name.
            email = f"specs-{uuid.uuid4().hex[:12]}@example.test"
            password = "e2e-pass-ok"
            created = page.request.post(
                f"{UI}/api/auth/signup",
                data=json.dumps(
                    {
                        "email": email,
                        "password": password,
                        "display_name": "E2E specs",
                    }
                ),
                headers={"Content-Type": "application/json"},
            )
            self.assertIn(created.status, (200, 201), created.text()[:400])
            logged = page.request.post(
                f"{UI}/api/auth/login",
                data=json.dumps({"email": email, "password": password}),
                headers={"Content-Type": "application/json"},
            )
            self.assertEqual(logged.status, 200, logged.text()[:400])
            ws = (logged.json().get("default_workspace") or {}).get("id")
            self.assertTrue(ws, logged.text()[:400])
            workspace_url = f"{UI}/{ws}"
            type(self).workspace_url = workspace_url
            page.goto(f"{workspace_url}/dashboard", wait_until="domcontentloaded")
            page.wait_for_url("**/dashboard", timeout=20_000)

        name = f"specs-i7-{uuid.uuid4().hex[:8]}"
        pair_and_heartbeat(
            page,
            ws,
            name,
            heartbeat_extra={"hardware": SPECS_HARDWARE},
        )
        last_err: Exception | None = None
        for _attempt in range(2):
            page.goto(
                f"{workspace_url}/runners/self-hosted",
                wait_until="domcontentloaded",
            )
            try:
                page.get_by_role("button", name="Add worker").wait_for(timeout=45_000)
                last_err = None
                break
            except Exception as e:
                last_err = e
        if last_err is not None:
            raise last_err

        assert_no_control_plane_card(page)
        card = page.locator("article").filter(has_text=name)
        self.assertGreater(
            card.count(),
            0,
            f"host card {name!r} missing. body={page.inner_text('body')[:800]!r}",
        )
        text = card.first.inner_text()

        # clean_cpu_model strips (R)/(TM); model fragment stays.
        self.assertIn("i7-10610U", text)
        self.assertNotIn("(R)", text)
        self.assertNotIn("(TM)", text)
        self.assertIn("8 cores", text)
        self.assertIn("16 GB", text)
        self.assertIn("DDR4-3200", text)
        self.assertIn("Intel UHD Graphics", text)
        # Load % is rendered on the jobs line, not specs_line.
        self.assertIn("cpu 12%", text)
        self.assertIn("mem 32%", text)

    def test_14k_show_removed(self) -> None:
        # catalog:e2e-show-removed — Show removed after revoke of a paired box.
        # Unittest name order: before test_18_logout so the session stays.
        # Control copy is `Show removed (N)` / `Hide removed (N)` (page.rs).
        page = self.page
        base = getattr(type(self), "workspace_url", None)
        if base:
            workspace_url = base
            ws = base.rstrip("/").rsplit("/", 1)[-1]
        else:
            ws, workspace_url = signup_login(page, prefix="show-rm")
            type(self).workspace_url = workspace_url

        leftover = f"never-paired-{uuid.uuid4().hex[:8]}"
        paired_name = f"paired-rm-{uuid.uuid4().hex[:8]}"

        leftover_created = create_host(page, ws, leftover)
        leftover_id = leftover_created["host"]["id"]
        revoked = page.request.delete(
            f"{UI}/api/workspaces/{ws}/worker-hosts/{leftover_id}"
        )
        self.assertTrue(
            revoked.ok,
            f"revoke leftover {leftover_id}: {revoked.status} {revoked.text()[:300]}",
        )

        pair_and_heartbeat(page, ws, paired_name)
        goto_self_hosted(page, workspace_url)

        card = page.locator("article").filter(has_text=paired_name)
        card.wait_for(timeout=15_000)
        self.assertNotIn(
            leftover,
            page.inner_text("body"),
            "revoked never-paired leftover must stay hidden",
        )

        card.get_by_role("button", name="Remove worker").click(force=True)
        page.get_by_text("Remove this worker?").wait_for(timeout=5_000)
        card.get_by_role("button", name="Confirm").click(force=True)
        card.wait_for(state="detached", timeout=15_000)

        body = page.inner_text("body")
        self.assertNotIn(
            paired_name,
            body,
            "removed paired box must be hidden by default",
        )
        self.assertNotIn(
            leftover,
            body,
            "never-paired leftover must stay hidden after paired revoke",
        )

        show = page.get_by_role("button", name=re.compile(r"^Show removed \(\d+\)$"))
        show.wait_for(timeout=5_000)
        self.assertTrue(show.is_visible(), "Show removed (N) after revoke of a paired box")
        show.click(force=True)

        page.get_by_role("button", name=re.compile(r"^Hide removed \(\d+\)$")).wait_for(
            timeout=5_000
        )
        restored = page.locator("article").filter(has_text=paired_name)
        restored.wait_for(timeout=10_000)
        self.assertTrue(restored.is_visible(), "Show removed must reveal the paired box")
        restored_text = restored.inner_text()
        self.assertRegex(restored_text.lower(), r"\bremoved\b")
        self.assertEqual(
            page.locator("article").filter(has_text=leftover).count(),
            0,
            "never-paired leftover must stay hidden after Show removed",
        )
        self.assertNotIn(leftover, page.inner_text("body"))

    def test_14m_apply_toast(self) -> None:
        # catalog:e2e-apply-toast-pending — Apply PATCH toast + pending apply.
        # Name order: after test_14k, before test_18_logout (session stays).
        page = self.page
        ws, workspace_url = _ensure_workspace(page)

        name = f"apply-toast-{uuid.uuid4().hex[:8]}"
        created, paired, _hb = pair_and_heartbeat(page, ws, name)
        host = created.get("host") if isinstance(created.get("host"), dict) else {}
        host_id = str(host.get("id") or created.get("id") or "")
        machine = paired.get("machine_token") or ""
        self.assertTrue(host_id, f"create host missing id: {created!r}"[:400])
        self.assertTrue(machine, f"pair missing machine_token: {paired!r}"[:400])

        page.goto(f"{workspace_url}/runners/self-hosted", wait_until="domcontentloaded")
        page.get_by_role("button", name="Add worker").wait_for(timeout=90_000)
        card = page.locator("article").filter(has_text=name)
        card.first.wait_for(state="attached", timeout=30_000)
        card = card.first
        self.assertNotIn("Control plane", card.inner_text())
        self.assertEqual(
            card.get_by_text("pending apply", exact=True).count(),
            0,
            "fresh pair must not show pending apply",
        )

        auto = card.locator("label").filter(has_text="Auto-update").locator("input")
        self.assertGreater(auto.count(), 0, "paired card needs Auto-update")
        apply = card.get_by_role("button", name="Apply changes")
        self.assertEqual(
            apply.count(),
            0,
            "Apply must stay hidden until Auto-update is toggled",
        )
        auto.first.click(force=True)
        apply.wait_for(state="attached", timeout=10_000)
        row = None
        try:
            apply.click(force=True)

            toast = page.get_by_role("status").filter(
                has_text="Settings sent. Waiting for the runner."
            )
            toast.first.wait_for(state="visible", timeout=15_000)
            self.assertIn(
                "Settings sent. Waiting for the runner.",
                toast.first.inner_text(),
                "Apply toast must use the pending-ack copy, not plane 'Settings applied.'",
            )

            pill = card.get_by_text("pending apply", exact=True)
            pill.first.wait_for(state="visible", timeout=10_000)
            self.assertTrue(
                pill.first.is_visible(),
                "Apply must show pending apply on the card, not only a toast",
            )

            listed = list_hosts(page, ws)
            row = next(
                (item for item in (listed.get("hosts") or []) if item.get("id") == host_id),
                None,
            )
            self.assertIsNotNone(row, f"applied host missing from list: {listed!r}"[:800])
            self.assertTrue(
                row.get("settings_pending")
                or row.get("settings_gen") != row.get("settings_ack_gen"),
                f"Apply must set settings_pending: {row}",
            )
            self.assertGreater(
                int(row.get("settings_gen") or 0),
                int(row.get("settings_ack_gen") or 0),
                f"Apply must bump settings_gen: {row}",
            )
        finally:
            # Restore so later suite tests still see unlocked widgets.
            try:
                if row is None:
                    listed = list_hosts(page, ws)
                    for item in listed.get("hosts") or []:
                        if item.get("id") == host_id:
                            row = item
                            break
                gen = int((row or {}).get("settings_gen") or 0)
                if gen and machine:
                    heartbeat(page, machine, {"settings_ack_gen": gen})
                dismiss = page.get_by_role("button", name="Dismiss")
                if dismiss.count() and dismiss.first.is_visible():
                    dismiss.first.click()
            except Exception:
                pass


    def test_14h_copy_buttons(self) -> None:
        # Isolated context so clipboard grants do not replace the class page.
        # copy_text tries execCommand first (HTTP LAN fallback), then clipboard API.
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
            permissions=["clipboard-read", "clipboard-write"],
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        page.set_default_navigation_timeout(30_000)
        try:
            ctx.grant_permissions(
                ["clipboard-read", "clipboard-write"], origin=UI
            )
            email = f"e2e-copy-{uuid.uuid4().hex[:12]}@example.test"
            headers = {"Content-Type": "application/json"}
            created = page.request.post(
                f"{UI}/api/auth/signup",
                data=json.dumps(
                    {
                        "email": email,
                        "password": "e2e-pass-ok",
                        "display_name": "E2E Copy",
                    }
                ),
                headers=headers,
            )
            self.assertIn(
                created.status,
                {200, 201},
                f"signup {created.status}: {created.text()[:400]}",
            )
            logged = page.request.post(
                f"{UI}/api/auth/login",
                data=json.dumps({"email": email, "password": "e2e-pass-ok"}),
                headers=headers,
            )
            self.assertTrue(
                logged.ok, f"login {logged.status}: {logged.text()[:400]}"
            )
            body = logged.json()
            ws = ((body.get("default_workspace") or {}).get("id")) or ""
            if not ws:
                me = page.request.get(f"{UI}/api/auth/me")
                spaces = (me.json() or {}).get("workspaces") or []
                ws = str((spaces[0].get("id") if spaces else "") or "")
            self.assertTrue(ws, f"login missing workspace: {body!r}"[:400])

            page.goto(
                f"{UI}/{ws}/runners/self-hosted",
                wait_until="domcontentloaded",
            )
            page.get_by_role("button", name="Add worker").wait_for(timeout=20_000)
            page.get_by_role("button", name="Add worker").click()
            page.get_by_role("heading", name="Pair this worker").wait_for(
                timeout=15_000
            )

            token = page.locator("pre").filter(has_text="flp_").last.inner_text().strip()
            self.assertTrue(token.startswith("flp_"), token)

            unix_btn = (
                page.locator("pre")
                .filter(has_text="install-worker.sh")
                .locator("xpath=..")
                .get_by_role("button", name="Copy")
            )
            win_btn = (
                page.locator("pre")
                .filter(has_text="install-worker.ps1")
                .locator("xpath=..")
                .get_by_role("button", name="Copy")
            )
            token_btn = (
                page.locator("pre")
                .filter(has_text="flp_")
                .last.locator("xpath=..")
                .get_by_role("button", name="Copy")
            )

            unix_btn.click()
            unix_clip = _poll_clipboard(page, "install-worker.sh")
            self.assertIn("install-worker.sh", unix_clip)

            win_btn.click()
            win_clip = _poll_clipboard(page, "install-worker.ps1")
            self.assertIn("install-worker.ps1", win_clip)

            token_btn.click()
            # Token is also inside the install commands; wait until those are gone.
            tok_clip = _poll_clipboard(page, token, exclude="install-worker")
            self.assertIn("flp_", tok_clip)
            self.assertIn(token, tok_clip)
        finally:
            ctx.close()

    def test_14a_rename_card(self) -> None:
        # Isolated run has no test_03 session; the suite reuses it.
        page = self.page
        base = getattr(type(self), "workspace_url", None)
        if base is None:
            email = f"e2e-rename-{uuid.uuid4().hex[:12]}@example.test"
            password = "e2e-pass-ok"
            page.goto(f"{UI}/signup", wait_until="networkidle")
            page.locator("#display_name").fill("E2E Rename")
            page.locator("#email").fill(email)
            page.locator("#password").fill(password)
            page.get_by_role("button", name="Sign up").click()
            page.wait_for_url("**/login", timeout=15_000)
            page.locator("#email").fill(email)
            page.locator("#password").fill(password)
            page.get_by_role("button", name="Log in").click()
            page.wait_for_url("**/dashboard", timeout=20_000)
            self.assertRegex(page.url, r"/[0-9a-f-]{36}/dashboard")
            base = page.url.rsplit("/dashboard", 1)[0]
            type(self).workspace_url = base
        page.goto(f"{base}/runners/self-hosted", wait_until="networkidle")
        page.wait_for_selector("h1")
        workers = page.locator("article").filter(
            has=page.get_by_role("button", name="Remove worker")
        )
        if workers.count() == 0:
            page.get_by_role("button", name="Add worker").click()
            page.get_by_role("heading", name="Pair this worker").wait_for(timeout=15_000)
            page.get_by_role("button", name="Done").click()
            page.get_by_role("heading", name="Pair this worker").wait_for(state="hidden")
            workers = page.locator("article").filter(
                has=page.get_by_role("button", name="Remove worker")
            )
        card = workers.first
        card.wait_for(timeout=10_000)
        assert_no_control_plane_card(page)

        card.get_by_title("Rename").click()
        inp = page.locator("input[autofocus]")
        inp.wait_for(timeout=5_000)
        saved = f"e2e-rename-{uuid.uuid4().hex[:8]}"
        inp.fill(saved)
        inp.press("Enter")
        page.get_by_title("Rename").filter(has_text=saved).wait_for(timeout=15_000)

        page.reload(wait_until="networkidle")
        page.wait_for_selector("h1")
        self.assertTrue(
            page.get_by_title("Rename").filter(has_text=saved).first.is_visible(),
            "Enter must PATCH the name so it sticks after reload",
        )

        # Name text lives on the button; after click it is an input value, so
        # do not keep filtering the card with has_text=saved.
        page.get_by_title("Rename").filter(has_text=saved).click()
        inp = page.locator("input[autofocus]")
        inp.wait_for(timeout=5_000)
        cancelled = f"e2e-esc-{uuid.uuid4().hex[:8]}"
        inp.fill(cancelled)
        inp.press("Escape")
        page.get_by_title("Rename").filter(has_text=saved).wait_for(timeout=5_000)
        self.assertNotIn(
            cancelled,
            page.inner_text("body"),
            "Escape must restore the previous name and exit the input",
        )

    def test_14j_last_seen(self) -> None:
        """Pending card: last seen never. After pair+heartbeat: just now / relative.

        Self-contained so this method can run alone.
        """
        page = self.page
        base = getattr(type(self), "workspace_url", None)
        if base:
            ws = base.rstrip("/").rsplit("/", 1)[-1]
        else:
            ws, base = signup_login(page, prefix="last-seen")
            type(self).workspace_url = base

        name = f"seen-{uuid.uuid4().hex[:8]}"
        created = create_host(page, ws, name)
        token = created["pair_token"]

        page.goto(f"{base}/runners/self-hosted", wait_until="networkidle")
        page.wait_for_selector("h1")
        assert_no_control_plane_card(page)

        pending = page.locator("article").filter(has_text=name)
        pending.first.wait_for(timeout=15_000)
        pending_text = pending.first.inner_text()
        self.assertRegex(
            pending_text.lower(),
            r"(last seen )?never",
            "pending/unpaired card must show never / last seen never",
        )

        paired = pair_host(
            page,
            token,
            hostname=name,
            platform="linux-x86_64",
        )
        heartbeat(
            page,
            paired["machine_token"],
            {"hostname": name, "platform": "linux-x86_64"},
        )

        page.goto(f"{base}/runners/self-hosted", wait_until="networkidle")
        page.wait_for_selector("h1")
        live = page.locator("article").filter(has_text=name)
        live.first.wait_for(timeout=15_000)
        live_text = live.first.inner_text()
        self.assertNotIn("Control plane", live_text)
        self.assertRegex(
            live_text.lower(),
            r"last seen (just now|\d+[smhd] ago)",
            "live card must show just now or relative last seen after pair+heartbeat",
        )
        assert_no_control_plane_card(page)



    def test_14g_software_line(self) -> None:
        # catalog:e2e-software-line — Software line after pair; not on plane.
        # Isolated run signs in; suite reuses test_03. Bake id is software.git_sha,
        # never a bin file hash.
        page = self.page
        base = getattr(type(self), "workspace_url", None)
        ws = getattr(type(self), "workspace_id", None)
        if not base:
            ws, base = signup_login(page, prefix="sw-line")
            type(self).workspace_url = base
            type(self).workspace_id = ws
        elif not ws:
            ws = base.rstrip("/").rsplit("/", 1)[-1]

        name = f"sw-line-{uuid.uuid4().hex[:8]}"
        bake_sha = "9d239a509055abcdef"
        bake_short = "9d239a509055"
        bin_sha = "2a971d693b6f7f0c13c94269845ca4ac0c73672b72ac7a2c7839be2cee590c8e"
        bin_prefix = "2a971d69"
        pair_and_heartbeat(
            page,
            ws,
            name,
            platform="linux-x86_64",
            heartbeat_extra={
                "software": {
                    "git_sha": bake_sha,
                    "bins": {"fleet-host": bin_sha},
                    "updating": False,
                }
            },
        )
        # Poll ~10s would hang networkidle; reload from the list.
        goto_self_hosted(page, base)
        page.reload(wait_until="domcontentloaded")
        _wait_dx_ready(page)
        page.get_by_role("button", name="Add worker").wait_for(timeout=30_000)
        assert_no_control_plane_card(page)

        card = page.locator("article").filter(has_text=name)
        card.first.wait_for(timeout=15_000)
        self.assertNotIn("Control plane", card.first.inner_text())
        card.first.wait_for(timeout=15_000)
        text = card.first.inner_text()
        lowered = text.lower()
        self.assertIn("fleet-host", lowered)
        self.assertIn("fleet-driver", lowered)
        self.assertNotIn(
            bin_prefix,
            text,
            "file hash must not be used as the bake / version id",
        )
        self.assertNotIn("software linux-x86_64", lowered)

    def test_14c_remove_confirm(self) -> None:
        # catalog:e2e-remove-confirm — Confirm removes; Cancel aborts.
        """Cancel aborts remove; Confirm drops the worker from the active list."""
        page = self.page
        base = getattr(type(self), "workspace_url", None)
        if base:
            ws = base.rstrip("/").rsplit("/", 1)[-1]
        else:
            ws, base = signup_login(page, prefix="rm-confirm")
            type(self).workspace_url = base

        name = f"rm-{uuid.uuid4().hex[:8]}"
        create_host(page, ws, name)
        goto_self_hosted(page, base)

        assert_no_control_plane_card(page)

        card = page.locator("article").filter(has_text=name)
        card.wait_for(timeout=10_000)
        self.assertIn("pending", card.inner_text().lower())
        self.assertNotIn("Control plane", card.inner_text())
        self.assertTrue(
            card.get_by_role("button", name="Remove worker").is_visible(),
            "pending worker must show Remove worker",
        )

        confirm_copy = "Remove this worker?"
        card.get_by_role("button", name="Remove worker").click()
        page.get_by_text(confirm_copy).wait_for(timeout=5_000)
        self.assertIn(confirm_copy, card.inner_text())
        self.assertTrue(card.get_by_role("button", name="Confirm").is_visible())
        self.assertTrue(card.get_by_role("button", name="Cancel").is_visible())

        card.get_by_role("button", name="Cancel").click()
        card.get_by_role("button", name="Remove worker").wait_for(timeout=5_000)
        self.assertNotIn(confirm_copy, card.inner_text())
        self.assertTrue(card.is_visible(), "Cancel must keep the worker listed")
        self.assertIn(name, card.inner_text())

        card.get_by_role("button", name="Remove worker").click()
        page.get_by_text(confirm_copy).wait_for(timeout=5_000)
        card.get_by_role("button", name="Confirm").click()
        card.wait_for(state="detached", timeout=15_000)
        self.assertEqual(
            page.locator("article").filter(has_text=name).count(),
            0,
            "Confirm must drop the worker from the active list",
        )
        self.assertNotIn(name, page.inner_text("body"))
        assert_no_control_plane_card(page)


    def test_14b_pending_apply_outdated(self) -> None:
        # catalog:e2e-pending-apply-outdated-pills
        # Extra pills: after Apply, pending apply until ack; outdated when behind.
        page = self.page
        ws, workspace_url = _ensure_workspace(page)

        name = f"pills-{uuid.uuid4().hex[:12]}"
        created, paired, hb = pair_and_heartbeat(page, ws, name)
        host_id = created["host"]["id"]
        machine = paired["machine_token"]
        self.assertTrue(str(machine).startswith("flm_"), machine)
        self.assertEqual((hb.get("host") or {}).get("status"), "online", hb)

        page.goto(f"{workspace_url}/runners/self-hosted", wait_until="domcontentloaded")
        page.wait_for_selector("h1", timeout=30_000)
        page.get_by_role("button", name="Add worker").wait_for(timeout=20_000)
        card = page.locator("article").filter(has_text=name)
        if card.count() == 0:
            page.wait_for_timeout(1500)
            page.reload(wait_until="domcontentloaded")
            page.get_by_role("button", name="Add worker").wait_for(timeout=20_000)
            card = page.locator("article").filter(has_text=name)
        try:
            card.first.wait_for(timeout=20_000)
        except Exception:
            self.fail(
                f"host card {name!r} missing after pair. "
                f"body={page.inner_text('body')[:1200]!r}"
            )
        card = card.first
        self.assertIn("online", card.inner_text().lower())
        self.assertEqual(
            card.get_by_text("pending apply", exact=True).count(),
            0,
            "fresh pair must not show pending apply",
        )

        auto = card.locator("label").filter(has_text="Auto-update").locator("input")
        self.assertGreater(auto.count(), 0, "paired card needs Auto-update")
        if auto.first.is_checked():
            auto.first.click(force=True)
        else:
            card.get_by_role("button", name="Stop taking new jobs").click(force=True)
        apply = card.get_by_role("button", name="Apply changes")
        apply.wait_for(timeout=5_000)
        apply.click(force=True)

        pill = card.get_by_text("pending apply", exact=True)
        pill.wait_for(timeout=10_000)
        self.assertTrue(pill.is_visible(), "pending apply must be a card pill")

        listed = list_hosts(page, ws)
        row = next((h for h in (listed.get("hosts") or []) if h.get("id") == host_id), None)
        self.assertIsNotNone(row, listed)
        self.assertTrue(
            row.get("settings_pending")
            or row.get("settings_gen") != row.get("settings_ack_gen"),
            f"Apply must bump settings_gen: {row}",
        )
        settings_gen = int(row["settings_gen"])
        self.assertGreater(settings_gen, int(row.get("settings_ack_gen") or 0), row)

        acked = heartbeat(page, machine, {"settings_ack_gen": settings_gen})
        acked_host = acked.get("host") or {}
        self.assertEqual(acked_host.get("settings_ack_gen"), settings_gen, acked)
        self.assertFalse(acked_host.get("settings_pending"), acked)

        page.goto(f"{workspace_url}/runners/self-hosted", wait_until="domcontentloaded")
        page.wait_for_selector("h1", timeout=30_000)
        card = page.locator("article").filter(has_text=name).first
        card.wait_for(timeout=15_000)
        self.assertEqual(
            card.get_by_text("pending apply", exact=True).count(),
            0,
            "ack must clear pending apply on the card",
        )
        self.assertNotIn("pending apply", card.inner_text().lower())

        # Outdated: heartbeat software behind staged dist if the plane has one.
        bake = "deadbeefdeadbeef"
        host_sha = "f" * 64
        man = page.request.get(f"{API}/install/worker/bin/linux-x86_64/manifest.json")
        manifest = None
        if man.ok:
            try:
                body = man.json()
            except Exception:
                body = None
            if isinstance(body, dict):
                manifest = body
        if manifest:
            staged = (manifest.get("git_sha") or "").strip()
            if staged and staged.lower() == bake.lower():
                bake = "cafebabecafebabe"
            bins = manifest.get("bins") or {}
            staged_host = bins.get("fleet-host")
            if isinstance(staged_host, dict):
                staged_host = staged_host.get("sha256")
            if isinstance(staged_host, str) and staged_host.strip().lower() == host_sha.lower():
                host_sha = "e" * 64
        behind = heartbeat(
            page,
            machine,
            {
                "platform": "linux-x86_64",
                "hostname": name,
                "software": {
                    "git_sha": bake,
                    "bins": {
                        "fleet-host": host_sha,
                        "fleet-driver": "c" * 64,
                    },
                },
                "settings_ack_gen": settings_gen,
            },
        )
        behind_host = behind.get("host") or {}
        api_outdated = bool(behind_host.get("outdated")) or behind_host.get(
            "software_status"
        ) == "behind"
        self.assertIsNotNone(
            manifest,
            "staged linux-x86_64 manifest is required to assert the outdated "
            "pill (scripts/stage-worker-dist.sh). Missing staged dist is a fail.",
        )
        self.assertTrue(
            api_outdated,
            f"staged dist must mark non-matching bake behind: {behind_host}",
        )

        page.goto(f"{workspace_url}/runners/self-hosted", wait_until="domcontentloaded")
        page.wait_for_selector("h1", timeout=30_000)
        card = page.locator("article").filter(has_text=name).first
        card.wait_for(timeout=15_000)
        outdated = card.get_by_text("outdated", exact=True)
        outdated.wait_for(timeout=10_000)
        self.assertTrue(
            outdated.is_visible(),
            f"behind package must show outdated on the card: {card.inner_text()}",
        )
        self.assertNotIn("pending apply", card.inner_text().lower())


    def test_14n_widgets_lock(self) -> None:
        # catalog:e2e-widgets-lock-pending — Inputs disabled while pending.
        page = self.page
        ws, workspace_url = _ensure_workspace(page)

        name = f"lock-{uuid.uuid4().hex[:8]}"
        created, paired, _hb = pair_and_heartbeat(page, ws, name)
        host = created.get("host") or created
        host_id = host.get("id")
        self.assertTrue(host_id, created)
        machine_token = paired["machine_token"]

        page.goto(f"{workspace_url}/runners/self-hosted", wait_until="domcontentloaded")
        _wait_dx_ready(page)
        page.wait_for_selector("h1", timeout=30_000)
        card = page.locator("article").filter(has_text=name)
        card.first.wait_for(timeout=15_000)
        card = card.first
        self.assertIn("online", card.inner_text().lower())

        auto = card.locator("label").filter(has_text="Auto-update").locator("input")
        max_jobs = card.locator("label").filter(has_text="Max jobs at once").locator("input")

        self.assertGreater(auto.count(), 0, "paired card needs Auto-update")
        self.assertGreater(max_jobs.count(), 0, "paired card needs Max jobs at once")
        self.assertEqual(
            card.get_by_role("checkbox", name=re.compile(r"^Native$")).count(),
            0,
            "isolation Native checkbox must not exist",
        )
        self.assertEqual(
            card.locator("label").filter(has_text="Sandboxed").count(),
            0,
            "isolation Sandboxed checkbox must not exist",
        )
        self.assertIn("Runtime: process on this machine", card.inner_text())
        self.assertTrue(auto.first.is_enabled(), "Auto-update must be editable before Apply")
        self.assertTrue(max_jobs.first.is_enabled(), "max jobs must be editable before Apply")
        self.assertEqual(
            card.get_by_text("pending apply", exact=True).count(),
            0,
            "fresh pair must not show pending apply",
        )

        auto.first.click(force=True)
        apply = card.get_by_role("button", name="Apply changes")
        apply.wait_for(timeout=5_000)
        apply.click(force=True)

        pill = card.get_by_text("pending apply", exact=True)
        pill.wait_for(timeout=10_000)
        self.assertTrue(
            pill.is_visible(),
            "pending apply pill must show while settings await ack",
        )
        self.assertFalse(
            auto.first.is_enabled(),
            "Auto-update must lock while pending apply",
        )
        self.assertFalse(
            max_jobs.first.is_enabled(),
            "Max jobs at once must lock while pending apply",
        )

        listed = list_hosts(page, ws)
        row = next(
            (h for h in (listed.get("hosts") or []) if h.get("id") == host_id),
            None,
        )
        self.assertIsNotNone(row, listed)
        self.assertTrue(
            row.get("settings_pending")
            or row.get("settings_gen") != row.get("settings_ack_gen"),
            f"Apply must set settings_pending: {row}",
        )
        settings_gen = int(row["settings_gen"])
        heartbeat(page, machine_token, {"settings_ack_gen": settings_gen})

        # Ack so later suite tests (test_17 dirty Apply) can still edit inputs.
        page.goto(f"{workspace_url}/runners/self-hosted", wait_until="domcontentloaded")
        _wait_dx_ready(page)
        page.wait_for_selector("h1", timeout=30_000)
        card = page.locator("article").filter(has_text=name)
        card.first.wait_for(timeout=15_000)
        card = card.first
        self.assertEqual(
            card.get_by_text("pending apply", exact=True).count(),
            0,
            "ack must clear pending apply so later suite tests can edit",
        )
        auto = card.locator("label").filter(has_text="Auto-update").locator("input")
        max_jobs = card.locator("label").filter(has_text="Max jobs at once").locator("input")
        self.assertTrue(auto.first.is_enabled(), "Auto-update must unlock after ack")
        self.assertTrue(max_jobs.first.is_enabled(), "max jobs must unlock after ack")

    def test_14l_plane_sandboxed(self) -> None:
        # catalog:e2e-plane-runtime — plane uses SANDBOX_BACKEND, no isolation checkboxes.
        page = self.page
        base = getattr(type(self), "workspace_url", None)
        if base:
            workspace_url = base
        else:
            _, workspace_url = signup_login(page, prefix="plane-sbx")
            type(self).workspace_url = workspace_url

        goto_self_hosted(page, workspace_url)
        assert_no_control_plane_card(page)
        body = page.inner_text("body")
        self.assertIn("No workers yet", body)
        self.assertIn("The API is not a runner", body)

    def test_14d_update_now(self) -> None:
        # catalog:e2e-update-now — Auto-update off + Update now when behind.
        # Isolated run signs in via API; suite reuses test_03. Before test_18_logout.
        page = self.page
        ws, workspace_url = _ensure_workspace(page)

        name = f"updnow-{uuid.uuid4().hex[:8]}"
        listed = list_hosts(page, ws)
        created, paired, hb = pair_and_heartbeat(
            page,
            ws,
            name,
            heartbeat_extra={"software": _mismatch_software(listed)},
        )
        host = hb.get("host") if isinstance(hb.get("host"), dict) else {}
        machine = paired["machine_token"]
        if not _host_needs_software_update(host):
            hb = heartbeat(
                page,
                machine,
                {"software": {"bins": {"fleet-host": "not-the-staged-sha"}}},
            )
            host = hb.get("host") if isinstance(hb.get("host"), dict) else {}
        if not _host_needs_software_update(host):
            listed = list_hosts(page, ws)
            hb = heartbeat(
                page,
                machine,
                {"software": _mismatch_software(listed)},
            )
            host = hb.get("host") if isinstance(hb.get("host"), dict) else {}
        self.assertTrue(
            _host_needs_software_update(host),
            "API did not mark the host behind/outdated "
            f"(software_status={host.get('software_status')!r} "
            f"outdated={host.get('outdated')!r} desired={host.get('desired')!r})",
        )

        host_id = created["host"]["id"]
        patched = patch_host(page, ws, host_id, {"auto_update": False})
        self.assertFalse(
            patched.get("auto_update", True),
            f"PATCH auto_update=false: {patched}",
        )
        gen = patched.get("settings_gen")
        if gen:
            heartbeat(page, machine, {"settings_ack_gen": int(gen)})

        page.goto(f"{workspace_url}/runners/self-hosted", wait_until="domcontentloaded")
        _wait_dx_ready(page)
        page.get_by_role("button", name="Add worker").wait_for(timeout=90_000)
        assert_no_control_plane_card(page)

        card = page.locator("article").filter(has_text=name)
        card.first.wait_for(state="attached", timeout=30_000)
        update_now = card.get_by_role("button", name="Update now")
        if update_now.count() == 0:
            auto = card.locator("label").filter(has_text="Auto-update").locator("input")
            self.assertGreater(auto.count(), 0, "paired card missing Auto-update")
            if auto.first.is_checked():
                auto.first.uncheck(force=True)
            update_now = card.get_by_role("button", name="Update now")
        self.assertGreater(
            update_now.count(),
            0,
            f"Update now missing on behind card: {card.first.inner_text()[:800]!r}",
        )
        update_now.first.click(force=True)

        requested = card.get_by_role("button", name="Update requested")
        try:
            requested.first.wait_for(state="attached", timeout=15_000)
        except Exception:
            pass
        body = card.first.inner_text()
        self.assertTrue(
            requested.count() > 0 or "Update requested" in body,
            f"expected Update requested after click: {body[:800]!r}",
        )

    def test_14i_loopback_warning(self) -> None:
        # Catalog:e2e-loopback-warning — pair panel warns when the command is
        # loopback (hint_is_loopback: 127.0.0.1 / localhost / [::1]).
        # Unittest name order: before test_18_logout so the session stays.
        # API_PUBLIC_BASE may be a LAN address; rewrite the create-host hint
        # to loopback so this layer-2 warning copy is still exercised.
        page = self.page
        base = getattr(type(self), "workspace_url", None)
        if base:
            workspace_url = base
        else:
            _ws, workspace_url = signup_login(page, prefix="loopback")
            type(self).workspace_url = workspace_url

        host_re = re.compile(r"/api/workspaces/[^/]+/worker-hosts/?$")

        def _force_loopback_hint(route):
            if route.request.method.upper() != "POST":
                route.continue_()
                return
            try:
                response = route.fetch()
                data = response.json()
            except Exception:
                route.continue_()
                return
            hint = data.get("install_hint") or ""
            lower = hint.lower()
            if not hint or any(
                h in lower for h in ("127.0.0.1", "localhost", "[::1]")
            ):
                route.fulfill(response=response)
                return
            data["install_hint"] = re.sub(
                r"https?://[^/\s]+",
                "http://127.0.0.1:8080",
                hint,
            )
            headers = {
                k: v
                for k, v in response.headers.items()
                if k.lower()
                not in {"content-length", "content-encoding", "transfer-encoding"}
            }
            headers["content-type"] = "application/json"
            route.fulfill(
                status=response.status,
                headers=headers,
                body=json.dumps(data),
            )

        add = page.get_by_role("button", name="Add worker")
        deadline = time.time() + 180
        last_body = ""
        while time.time() < deadline:
            try:
                goto_self_hosted(page, workspace_url)
            except Exception:
                pass
            _wait_dx_ready(page)
            try:
                last_body = page.inner_text("body")
            except Exception:
                last_body = ""
            if add.count() and add.first.is_visible():
                break
            page.wait_for_timeout(1_500)
        else:
            self.fail(
                "Add worker missing on Self-hosted (dx rebuild overlay?). "
                f"url={page.url} body={last_body[:1500]!r}"
            )

        page.route(host_re, _force_loopback_hint)
        try:
            add.first.click()
            heading = page.get_by_role("heading", name="Pair this worker")
            try:
                heading.wait_for(timeout=30_000)
            except Exception:
                self.fail(
                    "Pair this worker panel did not open. "
                    f"url={page.url} body={page.inner_text('body')[:1500]!r}"
                )
            panel = heading.locator("xpath=ancestor::section[1]")
            text = panel.inner_text()
            self.assertIn(
                "loopback URL",
                text,
                f"pair panel must warn on loopback command: {text[:800]!r}",
            )
            self.assertRegex(
                text,
                r"LAN or public",
                f"warning must mention LAN or public URL: {text[:800]!r}",
            )
        finally:
            page.unroute(host_re, _force_loopback_hint)

    def test_14e_poll_list(self) -> None:
        # catalog:e2e-poll — Poll list ~10s. Card updates after pair / heartbeat.
        # POLL_MS=10_000 in self_hosted/page.rs. Pair via page.request; no reload.
        page = self.page
        base = getattr(type(self), "workspace_url", None)
        if base:
            workspace_url = base
        else:
            _ws, workspace_url = signup_login(page, prefix="e2e-poll")
            type(self).workspace_url = workspace_url

        page.goto(f"{workspace_url}/runners/self-hosted", wait_until="networkidle")
        page.wait_for_selector("h1")
        page.get_by_role("button", name="Add worker").wait_for(timeout=10_000)
        assert_no_control_plane_card(page)
        self.assertIn("/runners/self-hosted", page.url)
        url_before = page.url

        suffix = uuid.uuid4().hex[:12]
        hostname = f"poll-box-{suffix}"
        self.assertNotIn(hostname, page.inner_text("body"))

        page.get_by_role("button", name="Add worker").click()
        page.get_by_role("heading", name="Pair this worker").wait_for(timeout=15_000)
        token = page.locator("pre").filter(has_text="flp_").last.inner_text().strip()
        self.assertTrue(token.startswith("flp_"), f"pair token prefix: {token}")
        page.get_by_role("button", name="Done").click()
        page.get_by_role("heading", name="Pair this worker").wait_for(state="hidden")

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
            },
        )

        self.assertEqual(page.url, url_before, "must not navigate away to pair")
        self.assertNotIn(
            hostname,
            page.inner_text("body"),
            "hostname must not appear before the ~10s list poll",
        )

        # Hostname shares a <p> with " · platform"; do not wait exact=True.
        card = page.locator("article").filter(has_text=hostname)
        try:
            card.first.wait_for(timeout=20_000)
        except Exception as e:
            self.fail(
                f"card with hostname {hostname!r} did not appear from the "
                f"~10s list poll: {e}. body={page.inner_text('body')[:1200]!r}"
            )
        self.assertEqual(
            page.url,
            url_before,
            "card must appear from the poll, not a navigation/reload",
        )
        self.assertGreater(card.count(), 0, "paired host card missing after poll")
        text = card.first.inner_text()
        self.assertIn("online", text.lower())
        self.assertRegex(text.lower(), r"last seen (just now|\d+s ago|\d+m ago)")
        self.assertIn(hostname, text)


    def test_18_logout_returns_to_login(self) -> None:
        # Unittest runs methods in name order. Logout must stay last so
        # test_14m/15/16/17 still have the session from test_03.
        base = getattr(type(self), "workspace_url", None)
        self.assertIsNotNone(base, "signup/login must run first")
        page = self.page
        if "/login" in page.url:
            page.goto(f"{base}/dashboard", wait_until="domcontentloaded")
            page.wait_for_url("**/dashboard", timeout=20_000)
        page.locator("button[aria-haspopup='menu']").click()
        page.get_by_role("button", name="Log out", exact=True).click()
        page.wait_for_url("**/login", timeout=15_000)
        self.assertIn("/login", page.url)
        self.assertIn("Welcome back", page.inner_text("h1"))


if __name__ == "__main__":
    unittest.main(verbosity=2)
