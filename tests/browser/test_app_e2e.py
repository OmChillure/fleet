#!/usr/bin/env python3
"""Headless Chromium e2e against the live UI (:3030) + API (:8080).

Install: crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh
  (ENABLE_BROWSER_E2E=1 in repo-root .env; PROFILE=full defaults it on).

Does not skip when the stack is down — fail so the suite cannot go green blind.
"""
from __future__ import annotations

import os
import socket
import sys
import unittest
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from stack_identity import ForeignStackError, require_this_tree  # noqa: E402

UI = os.environ.get("FLEET_UI_URL", "http://127.0.0.1:3030").rstrip("/")
API = os.environ.get("FLEET_API_URL", "http://127.0.0.1:8080").rstrip("/")
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
        self.assertEqual(self.page.inner_text("h1").strip(), "Dashboard")
        type(self).workspace_url = self.page.url.rsplit("/dashboard", 1)[0]

    def test_04_sidebar_app_pages(self) -> None:
        base = getattr(type(self), "workspace_url", None)
        self.assertIsNotNone(base, "signup/login must run first")
        page = self.page

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
        runners_body = page.inner_text("body")
        self.assertTrue(
            page.get_by_role("button", name="Add worker").count() > 0,
            "Self-hosted runners page missing Add worker",
        )
        self.assertTrue(
            "No workers yet" in runners_body or "last seen" in runners_body,
            "Self-hosted empty state or runner card missing",
        )

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
        self.assertIn("fleet-worker", body)

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
        self.assertNotIn("re-run the install command", card.lower())
        self.assertIn("Max jobs at once", card)
        self.assertIn("Auto-update", card)
        self.assertRegex(card, r"worker [0-9a-f]{8}")
        self.assertRegex(card.lower(), r"jobs\s+0\s*/\s*4")
        self.assertNotIn("Allow host", card)
        self.assertNotIn("Allow container", card)
        self.assertNotIn("Advertised", card)

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

    def test_10_logout_returns_to_login(self) -> None:
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
