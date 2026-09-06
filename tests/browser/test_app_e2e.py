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
    goto_self_hosted,
    list_hosts,
    pair_and_heartbeat,
    patch_host,
    signup_login,
    wait_dx_ready,
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


# First-send creates POST /chat/conversations then POST /chat or /chat/stream.
_CHAT_TURN_POST_RE = re.compile(
    r"/api/workspaces/[^/]+/chat(?:/stream|/conversations)?(?:\?|$)",
    re.I,
)


def _is_chat_turn_post(method: str, url: str) -> bool:
    """POST that would create a conversation or an empty-model turn."""
    if method.upper() != "POST":
        return False
    return _CHAT_TURN_POST_RE.search(url) is not None


def _json_provider(raw: str | None) -> str:
    if not raw:
        return ""
    try:
        body = json.loads(raw)
    except json.JSONDecodeError:
        return ""
    if not isinstance(body, dict):
        return ""
    p = body.get("provider")
    return p.strip() if isinstance(p, str) else ""


def _first_live_chat_provider(status: dict) -> str:
    """First ready provider that is not the OpenAI coming-soon placeholder."""
    for p in status.get("providers") or []:
        if not isinstance(p, dict):
            continue
        pid = str(p.get("id") or "").strip()
        if not pid or pid == "openai" or p.get("coming_soon"):
            continue
        if p.get("available"):
            return pid
    raise AssertionError(
        "no first available chat provider (excluding coming-soon openai): "
        f"{status!r}"[:800]
    )


def _chat_conversation_ids(page, workspace_id: str) -> list[str]:
    body = _json_ok(
        page.request.get(f"{UI}/api/workspaces/{workspace_id}/chat/conversations")
    )
    return _list_ids(body, "conversations")


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


_LIVE_SANDBOX_STATUSES = frozenset({"spawning", "healthy", "stuck", "killing"})
# Docs §6.6 / §17: cards are status-only. Cancel the run; do not kill here.
_SANDBOX_DESTROY_BTN = re.compile(
    r"\b(kill|destroy|terminate|reap|delete sandbox)\b",
    re.I,
)
# Docs §6.5: Run until done POSTs this at most 24 times, 400ms apart.
_FLEET_TICK_POST = re.compile(
    r"/api/workspaces/[^/]+/fleet/tick(?:/|$|\?)",
    re.I,
)
_RUN_TERMINAL = frozenset({"completed", "failed", "cancelled"})
_RUN_UNTIL_DONE_MAX_TICKS = 24
_RUN_UNTIL_DONE_GAP_MS = 400


def _json_ok(res) -> dict:
    text = res.text()
    if not res.ok:
        raise RuntimeError(f"{res.status} {res.url}: {text[:500]}")
    if not text:
        return {}
    return json.loads(text)


def _goto_ws_page(page, workspace_url: str, path: str) -> None:
    """Land on a workspace route. dx rebuild overlay can hide h1; retry."""
    last_err: Exception | None = None
    for _ in range(12):
        try:
            page.goto(
                f"{workspace_url}{path}",
                wait_until="domcontentloaded",
                timeout=15_000,
            )
            _wait_dx_ready(page)
            wait_dx_ready(page)
            page.locator("h1").first.wait_for(state="visible", timeout=8_000)
            last_err = None
            return
        except Exception as e:  # noqa: BLE001 — dx mid-rebuild; retry
            last_err = e
            time.sleep(2)
    raise RuntimeError(f"could not open {path}: {last_err}")


def _click_enabled(page, name: str) -> None:
    btn = page.get_by_role("button", name=name)
    btn.first.wait_for(state="visible", timeout=15_000)
    page.wait_for_function(
        """(label) => {
            const buttons = [...document.querySelectorAll('button')];
            const b = buttons.find((el) => (el.innerText || '').includes(label));
            return !!(b && !b.disabled);
        }""",
        arg=name,
        timeout=15_000,
    )
    btn.first.click()


def _pump_page_clock(page, ms: int) -> None:
    """Advance Playwright fake timers (gloo TimeoutFuture) and flush the page."""
    page.clock.run_for(ms)
    page.evaluate("void 0")


def _list_ids(body: dict, key: str) -> list[str]:
    rows = body.get(key) or []
    if not isinstance(rows, list):
        return []
    out: list[str] = []
    for row in rows:
        if isinstance(row, dict) and row.get("id"):
            out.append(str(row["id"]))
    return out


def _short_id(value: str) -> str:
    return value[:8] if len(value) >= 8 else value


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


# HTTP LAN (http://192.168.x.x) has no secure-context clipboard getter.
# Firefox panics if the getter is read; a silent return looks like a dead
# Copy button. copy_text must use textarea + execCommand first.
_INSTALL_EXEC_SPY = """() => {
  if (window.__fleetCopySpyInstalled) {
    window.__execCopies = [];
    return;
  }
  window.__execCopies = [];
  const proto = Document.prototype;
  const orig = proto.execCommand;
  proto.execCommand = function (cmd, showUI, value) {
    if (String(cmd).toLowerCase() === "copy") {
      const el = document.activeElement;
      let text = "";
      if (el && (el.tagName === "TEXTAREA" || el.tagName === "INPUT")) {
        text = el.value || "";
      } else {
        text = String(window.getSelection() || "");
      }
      window.__execCopies.push(text);
    }
    return orig.call(this, cmd, showUI, value);
  };
  window.__fleetCopySpyInstalled = true;
}"""

_STUB_CLIPBOARD_HTTP_LAN = """() => {
  const nav = window.navigator;
  if (window.__origClipboard === undefined) {
    window.__origClipboard = nav.clipboard;
  }
  window.__clipboardGets = 0;
  try {
    Object.defineProperty(nav, "clipboard", {
      configurable: true,
      get() {
        window.__clipboardGets += 1;
        throw new Error("clipboard getter forbidden on insecure HTTP LAN");
      },
    });
    return true;
  } catch (e) {
    try {
      delete nav.clipboard;
      return true;
    } catch (e2) {
      return false;
    }
  }
}"""

_RESTORE_CLIPBOARD_API = """() => {
  const orig = window.__origClipboard;
  if (orig === undefined) {
    return false;
  }
  try {
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      get() {
        return orig;
      },
    });
    return true;
  } catch (e) {
    return false;
  }
}"""


def _exec_copy_count(page) -> int:
    n = page.evaluate("() => (window.__execCopies || []).length")
    return int(n or 0)


def _clipboard_getter_hits(page) -> int:
    n = page.evaluate("() => Number(window.__clipboardGets || 0)")
    return int(n)


def _pair_panel(page):
    return page.get_by_role("heading", name="Pair this worker").locator(
        "xpath=ancestor::section[1]"
    )


def _pair_copy_button(page, pre_needle: str, *, token_only: bool = False):
    """Copy next to a pair-panel pre. Token-only skips the install commands."""
    loc = _pair_panel(page).locator("pre").filter(has_text=pre_needle)
    if token_only:
        loc = loc.filter(has_not_text="install-worker")
    return loc.first.locator("xpath=..").get_by_role("button", name="Copy")


def _open_pair_panel(page, workspace_url: str) -> None:
    add = page.get_by_role("button", name="Add worker")
    deadline = time.time() + 180
    last_body = ""
    last_err: Exception | None = None
    while time.time() < deadline:
        if not _listening("127.0.0.1", 3030) or not _listening("127.0.0.1", 8080):
            raise AssertionError(
                "stack died while opening pair panel "
                f"(ui={_listening('127.0.0.1', 3030)} api={_listening('127.0.0.1', 8080)})"
            )
        try:
            if "chrome-error" in (page.url or "") or not add.count():
                goto_self_hosted(page, workspace_url)
            wait_dx_ready(page, timeout_ms=3_000)
            last_body = page.inner_text("body")
            if add.count() and add.first.is_visible():
                add.first.click(force=True)
                page.get_by_role("heading", name="Pair this worker").wait_for(
                    timeout=20_000
                )
                return
        except Exception as e:
            last_err = e
        page.wait_for_timeout(500)
    raise AssertionError(
        "Add worker / Pair this worker missing (dx rebuild overlay?). "
        f"url={page.url} body={last_body[:1500]!r} err={last_err}"
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

    @classmethod
    def tearDownClass(cls) -> None:
        cls._context.close()
        cls._browser.close()
        cls._pw.stop()

    def test_01_landing_has_cta(self) -> None:
        from playwright.sync_api import expect

        page = self.page
        page.goto(f"{UI}/", wait_until="networkidle")
        get_started = page.get_by_role("link", name="Get started", exact=True)
        expect(get_started).to_be_visible()
        get_started.click()
        page.wait_for_url("**/signup", timeout=15_000)

        page.goto(f"{UI}/", wait_until="networkidle")
        log_in = page.get_by_role("main").get_by_role("link", name="Log in", exact=True)
        expect(log_in).to_be_visible()
        log_in.click()
        page.wait_for_url("**/login", timeout=15_000)

    def test_01_landing_navbar_dashboard_goes_to_login(self) -> None:
        from playwright.sync_api import expect

        # Own context: logged-out navbar Dashboard → /login (not /{uuid}/dashboard).
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            page.goto(f"{UI}/", wait_until="networkidle")
            dashboard = page.get_by_role("navigation").get_by_role(
                "link", name="Dashboard", exact=True
            )
            expect(dashboard).to_be_visible()
            dashboard.click()
            page.wait_for_url("**/login", timeout=15_000)
        finally:
            ctx.close()

    def test_02_unauthenticated_app_shell_redirects_to_login(self) -> None:
        # Own context: cookieless /{uuid}/dashboard → /login, not the class jar.
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            fake = uuid.uuid4()
            page.goto(f"{UI}/{fake}/dashboard", wait_until="domcontentloaded")
            page.wait_for_url("**/login", timeout=15_000)
            self.assertIn("/login", page.url)
        finally:
            ctx.close()

    def test_03_signup_then_login_reaches_dashboard(self) -> None:
        # Own context: chrome contract only — do not seed the class session.
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            email = f"e2e-{uuid.uuid4().hex[:12]}@example.test"
            password = "e2e-pass-ok"
            page.goto(f"{UI}/signup", wait_until="networkidle")
            page.get_by_label("Display name").fill("E2E Browser")
            page.get_by_label("Email").fill(email)
            page.get_by_label("Password").fill(password)
            page.get_by_role("button", name="Sign up").click()
            page.wait_for_url("**/login", timeout=15_000)
            self.assertIn("/login", page.url)
            self.assertNotRegex(page.url, r"/[0-9a-f-]{36}/dashboard")
            me = page.request.get(f"{UI}/api/auth/me")
            self.assertEqual(
                me.status,
                401,
                f"signup must not create a session: {me.status} {me.text()[:400]}",
            )

            page.get_by_label("Email").fill(email)
            page.get_by_label("Password").fill(password)
            page.get_by_role("button", name="Log in").click()
            page.wait_for_url("**/dashboard", timeout=20_000)
            self.assertRegex(page.url, r"/[0-9a-f-]{36}/dashboard")
            page.get_by_role("heading", name="Workforce", exact=True).wait_for(
                state="visible"
            )
            me = page.request.get(f"{UI}/api/auth/me")
            self.assertEqual(me.status, 200, me.text()[:400])
            body = me.json() or {}
            self.assertEqual((body.get("user") or {}).get("email"), email)
            ws = page.url.rsplit("/dashboard", 1)[0].rstrip("/").rsplit("/", 1)[-1]
            ids = [
                str(row.get("id") or "")
                for row in (body.get("workspaces") or [])
                if isinstance(row, dict)
            ]
            self.assertIn(
                ws,
                ids,
                f"dashboard workspace {ws} missing from /api/auth/me: {body!r}"[:400],
            )
        finally:
            ctx.close()

    def test_login_wrong_password_stays_on_login(self) -> None:
        # Unused email: HTTP 401 is fleet-auth; this locks chrome staying on /login.
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            email = f"e2e-{uuid.uuid4().hex[:12]}@example.test"
            page.goto(f"{UI}/login", wait_until="networkidle")
            page.get_by_label("Email").fill(email)
            page.get_by_label("Password").fill("wrong-password-xx")
            with page.expect_response(
                lambda r: "/api/auth/login" in r.url and r.request.method == "POST"
            ):
                page.get_by_role("button", name="Log in").click()
            self.assertIn("/login", page.url)
            self.assertNotRegex(page.url, r"/[0-9a-f-]{36}/dashboard")
            me = page.request.get(f"{UI}/api/auth/me")
            self.assertEqual(
                me.status,
                401,
                f"failed login must not create a session: {me.status} {me.text()[:400]}",
            )
        finally:
            ctx.close()

    def test_signed_in_settings_reload_stays_off_login(self) -> None:
        # Own signup: first-party cookie on :3030 must survive reload (not /login).
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            _ws, base = signup_login(page, prefix="set-reload")
            _goto_ws_page(page, base, "/settings")
            self.assertEqual(page.locator("h1").first.inner_text().strip(), "Settings")
            page.reload(wait_until="networkidle")
            self.assertNotIn("/login", page.url)
        finally:
            ctx.close()

    def test_sidebar_integrations_click_lands_on_integrations(self) -> None:
        # Own signup: Workspace accordion → Integrations hop (test_07 only sees the link).
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            signup_login(page, prefix="sb-integ")
            integ = page.get_by_role("link", name="Integrations", exact=True)
            if not integ.is_visible():
                # Workspace switcher is also a button named Workspace; the
                # accordion is the one with the ▸/▾ chevron.
                page.get_by_role("button", name="Workspace ▸").click()
                integ.wait_for(state="visible", timeout=10_000)
            integ.click()
            page.wait_for_url("**/integrations", timeout=10_000)
            page.wait_for_selector("h1")
            self.assertEqual(
                page.locator("h1").first.inner_text().strip(),
                "Integrations",
            )
        finally:
            ctx.close()

    def test_sidebar_new_chat_shows_provider_or_offline_not_stub(self) -> None:
        # Own context: New chat hop without pairing — live composer, not stub fiction.
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            signup_login(page, prefix="sb-chat")
            page.get_by_role("link", name="New chat", exact=True).click()
            page.wait_for_url("**/chat", timeout=10_000)
            _wait_dx_ready(page)
            wait_dx_ready(page)
            page.wait_for_function(
                """() => {
                    const sel = document.querySelector("select[title='Chat provider']");
                    const body = (document.body && document.body.innerText) || "";
                    return !!sel || body.includes("No chat provider ready");
                }""",
                timeout=10_000,
            )
            has_provider = page.locator("select[title='Chat provider']").count() > 0
            has_offline = "No chat provider ready" in page.inner_text("body")
            self.assertTrue(
                has_provider or has_offline,
                "new chat chrome missing (provider select or offline hint)",
            )
        finally:
            ctx.close()

    def test_05_workers_add_shows_pair_token(self) -> None:
        # Own signup: Add worker paints flp_ once in both install pres; Done+reload hide it.
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            _ws, base = signup_login(page, prefix="pair-tok")
            goto_self_hosted(page, base)
            page.get_by_role("button", name="Add worker").click()
            page.get_by_role("heading", name="Pair this worker").wait_for(timeout=15_000)

            unix = page.locator("pre").filter(has_text="install-worker.sh").inner_text()
            win = page.locator("pre").filter(has_text="install-worker.ps1").inner_text()
            token = page.locator("pre").filter(has_text="flp_").last.inner_text().strip()
            self.assertIn("FLEET_PAIR_TOKEN=", unix)
            self.assertIn("FLEET_PAIR_TOKEN=", win)
            self.assertTrue(token.startswith("flp_"), f"pair token prefix: {token}")
            self.assertGreaterEqual(len(token), len("flp_") + 32, token)
            self.assertIn(token, unix)
            self.assertIn(token, win)

            page.get_by_role("button", name="Done", exact=True).click()
            page.get_by_role("heading", name="Pair this worker").wait_for(state="hidden")
            self.assertNotIn(token, page.inner_text("body"))
            page.reload(wait_until="networkidle")
            wait_dx_ready(page)
            page.wait_for_selector("h1")
            self.assertNotIn(token, page.inner_text("body"))
        finally:
            ctx.close()

    def test_self_hosted_header_shows_current_package_semver(self) -> None:
        # Isolated visit: banner is Current fleet-host/driver version, not leftover names.
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            _ws, base = signup_login(page, prefix="pkg-ver")
            goto_self_hosted(page, base)
            page.get_by_text(
                re.compile(r"Current fleet-host version \d+\.\d+\.\d+")
            ).wait_for(timeout=15_000)
            body = page.inner_text("body")
            self.assertRegex(body, r"Current fleet-host version \d+\.\d+\.\d+")
            self.assertRegex(body, r"Current fleet-driver version \d+\.\d+\.\d+")
            self.assertNotIn("Current runner build", body)
            self.assertNotIn("Served packages", body)
        finally:
            ctx.close()

    def test_pending_card_paints_runtime_jobs_id_without_isolation(self) -> None:
        # Isolated Add+Done: Waiting-to-pair article paints runtime/jobs/id; no Native.
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            _ws, base = signup_login(page, prefix="pend-card")
            goto_self_hosted(page, base)
            page.get_by_role("button", name="Add worker").click()
            page.get_by_role("heading", name="Pair this worker").wait_for(timeout=15_000)
            page.get_by_role("button", name="Done", exact=True).click()
            page.get_by_role("heading", name="Pair this worker").wait_for(state="hidden")
            pending = page.get_by_role("button").filter(has_text="pending")
            pending.first.wait_for(timeout=10_000)
            pending.first.click()
            article = page.locator("article").filter(has_text="Waiting to pair")
            article.first.wait_for(timeout=10_000)
            text = article.first.inner_text()
            self.assertIn("Runtime: fleet-host", text)
            self.assertRegex(text.lower(), r"jobs\s+0\s*/\s*4")
            self.assertRegex(text, r"id [0-9a-f]{8}")
            self.assertEqual(
                article.first.get_by_role(
                    "checkbox", name=re.compile(r"^Native$")
                ).count(),
                0,
            )
        finally:
            ctx.close()

    def test_15_cancel_discards_pending_worker(self) -> None:
        # Isolated signup: pair-panel Cancel DELETEs unused pending leftover.
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            ws, base = signup_login(page, prefix="pair-cancel")
            goto_self_hosted(page, base)
            heading = page.get_by_role("heading", name="Pair this worker")
            page.get_by_role("button", name="Add worker").click()
            heading.wait_for(timeout=15_000)

            listed = list_hosts(page, ws)
            created = [
                h
                for h in (listed.get("hosts") or [])
                if isinstance(h, dict) and h.get("id") and h.get("status") == "pending"
            ]
            self.assertEqual(
                len(created),
                1,
                f"Add worker must create exactly one pending host: {listed!r}"[:800],
            )
            hid = str(created[0]["id"])

            with page.expect_response(
                lambda r: r.request.method.upper() == "DELETE"
                and f"/worker-hosts/{hid}" in r.url,
                timeout=15_000,
            ) as revoke:
                page.get_by_role("button", name="Cancel", exact=True).click()
            self.assertTrue(
                revoke.value.ok,
                f"Cancel DELETE leftover: {revoke.value.status}",
            )
            heading.wait_for(state="hidden", timeout=10_000)
            page.get_by_text("No workers yet").wait_for(timeout=10_000)
            self.assertEqual(
                page.get_by_role("button").filter(has_text="pending").count(),
                0,
                "Cancel leftover must not stay as a pending list card",
            )
            self.assertEqual(
                page.locator("article").filter(has_text="Waiting to pair").count(),
                0,
                "Cancel leftover must not stay as a Waiting-to-pair card",
            )
            self.assertEqual(
                page.get_by_role("button", name=re.compile(r"^Show removed")).count(),
                0,
                "never-paired Cancel leftover must stay off Show removed",
            )

            detail = page.request.get(f"{UI}/api/workspaces/{ws}/worker-hosts/{hid}")
            self.assertEqual(
                detail.status,
                200,
                f"GET leftover after Cancel: {detail.status} {detail.text()[:400]}",
            )
            body = detail.json()
            self.assertEqual(body.get("status"), "disabled", body)
            self.assertFalse(body.get("paired_at"), body)

            leftover = next(
                (
                    h
                    for h in (list_hosts(page, ws).get("hosts") or [])
                    if str(h.get("id")) == hid
                ),
                None,
            )
            self.assertIsNotNone(
                leftover,
                f"GET list must still include disabled leftover {hid}",
            )
            self.assertEqual(leftover.get("status"), "disabled", leftover)
            self.assertFalse(leftover.get("paired_at"), leftover)
        finally:
            ctx.close()

    def test_cancel_second_add_leaves_prior_done_pending(self) -> None:
        # After Done keeps A, Cancel of a second Add must leave A pending.
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            ws, base = signup_login(page, prefix="cancel-2nd")
            goto_self_hosted(page, base)
            heading = page.get_by_role("heading", name="Pair this worker")

            page.get_by_role("button", name="Add worker").click()
            heading.wait_for(timeout=15_000)
            listed = list_hosts(page, ws)
            first = [
                h
                for h in (listed.get("hosts") or [])
                if isinstance(h, dict) and h.get("id") and h.get("status") == "pending"
            ]
            self.assertEqual(
                len(first),
                1,
                f"first Add must create host A: {listed!r}"[:800],
            )
            host_a = str(first[0]["id"])
            page.get_by_role("button", name="Done", exact=True).click()
            heading.wait_for(state="hidden", timeout=10_000)

            page.get_by_role("button", name="Add worker").click()
            heading.wait_for(timeout=15_000)
            listed = list_hosts(page, ws)
            second = [
                h
                for h in (listed.get("hosts") or [])
                if isinstance(h, dict)
                and h.get("id")
                and h.get("status") == "pending"
                and str(h.get("id")) != host_a
            ]
            self.assertEqual(
                len(second),
                1,
                f"second Add must create host B: {listed!r}"[:800],
            )
            host_b = str(second[0]["id"])

            with page.expect_response(
                lambda r: r.request.method.upper() == "DELETE"
                and f"/worker-hosts/{host_b}" in r.url,
                timeout=15_000,
            ) as revoke:
                page.get_by_role("button", name="Cancel", exact=True).click()
            self.assertTrue(
                revoke.value.ok,
                f"Cancel DELETE B: {revoke.value.status}",
            )
            heading.wait_for(state="hidden", timeout=10_000)

            got_a = page.request.get(f"{UI}/api/workspaces/{ws}/worker-hosts/{host_a}")
            self.assertEqual(
                got_a.status,
                200,
                f"GET host A after Cancel B: {got_a.status} {got_a.text()[:400]}",
            )
            body_a = got_a.json()
            self.assertEqual(body_a.get("status"), "pending", body_a)

            got_b = page.request.get(f"{UI}/api/workspaces/{ws}/worker-hosts/{host_b}")
            self.assertEqual(
                got_b.status,
                200,
                f"GET host B after Cancel: {got_b.status} {got_b.text()[:400]}",
            )
            self.assertEqual(got_b.json().get("status"), "disabled", got_b.json())

            card = page.get_by_role("button").filter(has_text="pending")
            card.first.wait_for(timeout=10_000)
            self.assertIn("pending", card.first.inner_text().lower())
        finally:
            ctx.close()

    def test_fleet_run_until_done_caps_24_ticks(self) -> None:
        # catalog:list-fleet-run-until-done-caps-24-ticks
        # Docs §6.5: max 24 ticks, 400ms apart. Must not loop forever if a
        # task stays ready. Own context: signup must not clobber the suite
        # session / test_18 logout.
        context = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = context.new_page()
        page.set_default_timeout(20_000)
        try:
            self._assert_fleet_run_until_done_caps_24_ticks(page)
        finally:
            context.close()

    def _open_fleet_page(self, page, prefix: str):
        ws, base = signup_login(page, prefix=prefix)
        _goto_ws_page(page, base, "/fleet")
        self.assertEqual(page.locator("h1").first.inner_text().strip(), "Fleet")
        self.assertGreater(
            page.get_by_role("button", name="Run until done").count(),
            0,
            "Fleet page missing Run until done",
        )
        return ws

    def _pump_until_tick_posts(self, page, tick_at: list, n: int) -> None:
        deadline = time.monotonic() + 20
        while len(tick_at) < n:
            if time.monotonic() >= deadline:
                self.fail(
                    "Run until done POSTed "
                    f"{len(tick_at)}/{n} /fleet/tick under the fake clock"
                )
            _pump_page_clock(page, _RUN_UNTIL_DONE_GAP_MS)

    def _wait_run_until_done_enabled(self, page) -> None:
        page.wait_for_function(
            """() => {
                const buttons = [...document.querySelectorAll('button')];
                const b = buttons.find((el) =>
                    (el.innerText || '').includes('Run until done'));
                return !!(b && !b.disabled);
            }""",
            timeout=15_000,
        )

    def _assert_fleet_run_until_done_caps_24_ticks(self, page) -> None:
        ws = self._open_fleet_page(page, "fleet-cap24")

        # No paired host: tasks stay ready so the run cannot go terminal.
        # Fulfill tick locally so a background ticker 409 cannot abort the
        # loop before the 24-tick cap (docs: concurrent tick → 409 → stop).
        tick_at: list[float] = []

        def _fulfill_tick(route) -> None:
            req = route.request
            if req.method.upper() != "POST":
                route.continue_()
                return
            tick_at.append(time.time())
            route.fulfill(
                status=200,
                content_type="application/json",
                body=json.dumps(
                    {
                        "spawned": 0,
                        "completed_via_runtime": 0,
                        "runs_terminated": 0,
                    }
                ),
            )

        page.route(_FLEET_TICK_POST, _fulfill_tick)
        try:
            _click_enabled(page, "Start 2-task run")
            page.get_by_text("Started run").wait_for(timeout=15_000)

            runs_body = _json_ok(
                page.request.get(f"{UI}/api/workspaces/{ws}/fleet/runs")
            )
            run_rows = [
                r
                for r in (runs_body.get("runs") or [])
                if isinstance(r, dict) and r.get("id")
            ]
            self.assertTrue(run_rows, f"fixture must create a run: {runs_body!r}"[:800])
            self.assertFalse(
                all(
                    str(r.get("status") or "") in _RUN_TERMINAL for r in run_rows
                ),
                "cap lock needs an active run (do not pair a host): "
                f"{runs_body!r}"[:800],
            )

            # Install after chrome is live so WASM boot uses real timers.
            page.clock.install()
            _click_enabled(page, "Run until done")
            self._pump_until_tick_posts(
                page, tick_at, _RUN_UNTIL_DONE_MAX_TICKS
            )
            for _ in range(20):
                _pump_page_clock(page, 50)
            page.get_by_text(re.compile(r"Stopped after 24 ticks")).wait_for(
                timeout=10_000
            )

            self.assertEqual(
                len(tick_at),
                _RUN_UNTIL_DONE_MAX_TICKS,
                "Run until done must POST /fleet/tick exactly 24 times when "
                f"the run stays active, got {len(tick_at)}",
            )

            still = _json_ok(
                page.request.get(f"{UI}/api/workspaces/{ws}/fleet/runs")
            )
            still_rows = [
                r
                for r in (still.get("runs") or [])
                if isinstance(r, dict) and r.get("id")
            ]
            self.assertTrue(still_rows, f"run must still exist: {still!r}"[:800])
            self.assertFalse(
                all(
                    str(r.get("status") or "") in _RUN_TERMINAL for r in still_rows
                ),
                "run must still be active so the stop is the 24-tick cap: "
                f"{still!r}"[:800],
            )

            self._wait_run_until_done_enabled(page)
            _pump_page_clock(page, _RUN_UNTIL_DONE_GAP_MS * 4)
            self.assertEqual(
                len(tick_at),
                _RUN_UNTIL_DONE_MAX_TICKS,
                "must not POST a 25th tick after the cap: "
                f"got {len(tick_at)}",
            )
        finally:
            page.unroute(_FLEET_TICK_POST, _fulfill_tick)

    def test_run_until_done_stops_and_shows_tick_error_on_409(self) -> None:
        # Docs §16: a 409 tick (lock held) must show the error and stop
        # the loop before the 24-tick cap.
        context = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = context.new_page()
        page.set_default_timeout(20_000)
        tick_at: list[float] = []
        err = "project tick lock held"

        def _fulfill_tick_409(route) -> None:
            req = route.request
            if req.method.upper() != "POST":
                route.continue_()
                return
            tick_at.append(time.time())
            route.fulfill(
                status=409,
                content_type="application/json",
                body=json.dumps({"error": err}),
            )

        try:
            self._open_fleet_page(page, "fleet-tick409")
            page.route(_FLEET_TICK_POST, _fulfill_tick_409)
            _click_enabled(page, "Run until done")
            page.get_by_text(err).wait_for(timeout=10_000)
            self._wait_run_until_done_enabled(page)
            self.assertEqual(
                len(tick_at),
                1,
                "409 on the first tick must stop the loop: "
                f"got {len(tick_at)} POSTs",
            )
        finally:
            context.close()

    def test_run_until_done_stops_when_all_runs_terminal(self) -> None:
        # After a tick whose GET runs are all completed|failed|cancelled,
        # chrome says Run finished and no further POSTs.
        context = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = context.new_page()
        page.set_default_timeout(20_000)
        tick_at: list[float] = []
        runs_re = re.compile(
            r"/api/workspaces/[^/]+/fleet/runs(?:\?|$)",
            re.I,
        )

        def _fulfill_tick(route) -> None:
            req = route.request
            if req.method.upper() != "POST":
                route.continue_()
                return
            tick_at.append(time.time())
            route.fulfill(
                status=200,
                content_type="application/json",
                body=json.dumps(
                    {
                        "spawned": 0,
                        "completed_via_runtime": 0,
                        "runs_terminated": 0,
                    }
                ),
            )

        try:
            ws = self._open_fleet_page(page, "fleet-done1")
            _click_enabled(page, "Start 2-task run")
            page.get_by_text("Started run").wait_for(timeout=15_000)
            runs_body = _json_ok(
                page.request.get(f"{UI}/api/workspaces/{ws}/fleet/runs")
            )
            run_rows = [
                r
                for r in (runs_body.get("runs") or [])
                if isinstance(r, dict) and r.get("id")
            ]
            self.assertTrue(run_rows, f"fixture must create a run: {runs_body!r}"[:800])
            terminal_body = {
                "runs": [{**r, "status": "completed"} for r in run_rows]
            }

            def _fulfill_runs_terminal(route) -> None:
                if route.request.method.upper() != "GET":
                    route.continue_()
                    return
                route.fulfill(
                    status=200,
                    content_type="application/json",
                    body=json.dumps(terminal_body),
                )

            page.route(_FLEET_TICK_POST, _fulfill_tick)
            page.route(runs_re, _fulfill_runs_terminal)
            _click_enabled(page, "Run until done")
            page.get_by_text(re.compile(r"Run finished after 1 tick")).wait_for(
                timeout=10_000
            )
            self._wait_run_until_done_enabled(page)
            self.assertEqual(
                len(tick_at),
                1,
                "all-terminal GET runs must stop after that tick: "
                f"got {len(tick_at)} POSTs",
            )
        finally:
            context.close()

    def test_09_runners_redirects_to_self_hosted(self) -> None:
        # catalog:e2e-route-nav-redirects — /{ws}/runners lands on Self-hosted.
        _ws, base = _ensure_workspace(self.page)
        page = self.page
        page.goto(f"{base}/runners", wait_until="domcontentloaded")
        page.wait_for_url("**/runners/self-hosted", timeout=15_000)
        heading = page.locator("main h1").first
        heading.wait_for(state="visible")
        self.assertEqual(heading.inner_text().strip(), "Self-hosted")

    def test_10_nav_link_to_cloud(self) -> None:
        # Own signup: Runners accordion Cloud hop (test_11 only goto's the URL).
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            signup_login(page, prefix="nav-cloud")
            nav = page.locator("aside nav")
            cloud = nav.get_by_role("link", name="Cloud", exact=True)
            if cloud.count() == 0 or not cloud.first.is_visible():
                nav.get_by_text("Runners", exact=True).first.click()
                cloud = nav.get_by_role("link", name="Cloud", exact=True)
            self.assertTrue(cloud.first.is_visible(), "sidebar missing Cloud link")
            cloud.first.click()
            page.wait_for_url("**/runners/cloud", timeout=10_000)
            self.assertRegex(page.url, r"/[0-9a-f-]{36}/runners/cloud(?:[?#].*)?$")
            heading = page.locator("main h1").first
            heading.wait_for(state="visible")
            self.assertEqual(heading.inner_text().strip(), "Cloud")
        finally:
            ctx.close()

    def test_10_nav_link_to_partners(self) -> None:
        # Own signup: Runners accordion Partners hop (test_12 only goto's the URL).
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            signup_login(page, prefix="nav-ptn")
            nav = page.locator("aside nav")
            partners = nav.get_by_role("link", name="Partners", exact=True)
            if partners.count() == 0 or not partners.first.is_visible():
                nav.get_by_text("Runners", exact=True).first.click()
                partners = nav.get_by_role("link", name="Partners", exact=True)
            self.assertTrue(
                partners.first.is_visible(), "sidebar missing Partners link"
            )
            partners.first.click()
            page.wait_for_url("**/runners/partners", timeout=10_000)
            self.assertRegex(page.url, r"/[0-9a-f-]{36}/runners/partners(?:[?#].*)?$")
            heading = page.locator("main h1").first
            heading.wait_for(state="visible")
            self.assertEqual(heading.inner_text().strip(), "Partners")
        finally:
            ctx.close()

    def test_14h_copy_buttons_http_lan_fallback(self) -> None:
        # Isolated context so clipboard grants do not replace the class page.
        # copy_text tries execCommand first (HTTP LAN fallback), then clipboard API.
        # A getter that throws is Firefox on http://192.168.x.x: reading
        # navigator.clipboard panics; a silent no-op looks like a dead button.
        # Trying the API first via Reflect.get would still increment
        # __clipboardGets — that is a fail even if execCommand later succeeds.
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        page.set_default_navigation_timeout(30_000)
        page_errors: list[str] = []
        page.on(
            "pageerror",
            lambda err: page_errors.append(str(err)),
        )
        try:
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

            _open_pair_panel(page, f"{UI}/{ws}")

            btn = _pair_copy_button(page, "install-worker.sh")
            self.assertTrue(btn.is_visible(), "Unix Copy missing")

            page.evaluate(_INSTALL_EXEC_SPY)
            self.assertTrue(
                page.evaluate(_STUB_CLIPBOARD_HTTP_LAN),
                "must hide navigator.clipboard to exercise HTTP LAN fallback",
            )
            page_errors.clear()

            before = _exec_copy_count(page)
            btn.click()
            self.assertEqual(
                _exec_copy_count(page),
                before + 1,
                "each Copy click must call document.execCommand('copy') once",
            )
            self.assertEqual(
                _clipboard_getter_hits(page),
                0,
                "HTTP LAN must not read navigator.clipboard "
                "(Firefox getter throws; Reflect-then-fallback is still a fail)",
            )
            clip_errs = [
                e
                for e in page_errors
                if "clipboard" in e.lower() or "panic" in e.lower()
            ]
            self.assertEqual(
                clip_errs,
                [],
                f"HTTP LAN Copy must not panic: {clip_errs}",
            )

            # Secure-context still uses execCommand first. Clipboard-API-only
            # would write via writeText and leave __execCopies unchanged.
            self.assertTrue(
                page.evaluate(_RESTORE_CLIPBOARD_API),
                "must restore navigator.clipboard after the LAN probe",
            )
            before = _exec_copy_count(page)
            btn.click()
            self.assertEqual(
                _exec_copy_count(page),
                before + 1,
                "Copy must still use execCommand when navigator.clipboard exists",
            )
        finally:
            ctx.close()

    def test_sandboxes_page_matches_sessions(self) -> None:
        # catalog:e2e-sandboxes-page-matches-sessions
        # After pair+tick, /sandboxes cards show session/run/task short ids
        # and Chat /sessions does not. Live cards have no destroy (1367).
        # Own context: signup must not clobber the suite session / test_18 logout.
        context = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = context.new_page()
        page.set_default_timeout(20_000)
        try:
            self._assert_sandboxes_page_matches_sessions(page)
        finally:
            context.close()

    def _start_live_sandbox_sessions(self, page, *, prefix: str):
        """Pair a host and tick Fleet until a live worker session exists."""
        ws, base = signup_login(page, prefix=prefix)
        # Production never local-spawns (allow_local_spawn is test-only).
        pair_and_heartbeat(page, ws, f"{prefix}-{uuid.uuid4().hex[:8]}")
        _goto_ws_page(page, base, "/fleet")
        self.assertEqual(page.locator("h1").first.inner_text().strip(), "Fleet")
        _click_enabled(page, "Start 2-task run")
        page.get_by_text("Started run").wait_for(timeout=15_000)
        _click_enabled(page, "Run tick")
        sessions_body: dict = {}
        session_rows: list[dict] = []
        deadline = time.time() + 45
        while time.time() < deadline:
            sessions_body = _json_ok(
                page.request.get(f"{UI}/api/workspaces/{ws}/fleet/sessions")
            )
            raw = sessions_body.get("sessions") or []
            session_rows = [r for r in raw if isinstance(r, dict) and r.get("id")]
            if session_rows and any(
                str(r.get("status") or "") in _LIVE_SANDBOX_STATUSES
                for r in session_rows
            ):
                break
            tick = page.get_by_role("button", name="Run tick")
            if tick.count() and tick.first.is_enabled():
                tick.first.click()
            page.wait_for_timeout(800)
        self.assertTrue(
            session_rows,
            "Start 2-task run + Run tick must create fleet sessions: "
            f"{sessions_body!r}"[:800],
        )
        return ws, base, session_rows

    def _wait_sandbox_cards(self, page, ws: str, base: str) -> tuple[str, list[dict]]:
        """Refresh /sandboxes until each fleet session short id is on a card."""
        _goto_ws_page(page, base, "/sandboxes")
        _click_enabled(page, "Refresh")
        cards = ""
        session_rows: list[dict] = []
        deadline = time.time() + 20
        while time.time() < deadline:
            sessions_body = _json_ok(
                page.request.get(f"{UI}/api/workspaces/{ws}/fleet/sessions")
            )
            raw = sessions_body.get("sessions") or []
            session_rows = [r for r in raw if isinstance(r, dict) and r.get("id")]
            cards = page.inner_text("body")
            if session_rows and all(
                re.search(
                    rf"session\s+{re.escape(_short_id(str(s['id'])))}",
                    cards,
                )
                for s in session_rows
            ):
                break
            refresh = page.get_by_role("button", name="Refresh")
            if refresh.count() and refresh.first.is_enabled():
                refresh.first.click()
            page.wait_for_timeout(500)
        self.assertTrue(session_rows, "sessions list missing ids after refresh")
        return cards, session_rows

    def _assert_sandboxes_page_matches_sessions(self, page) -> None:
        ws, base, _seeded = self._start_live_sandbox_sessions(page, prefix="sbx-list")
        cards, session_rows = self._wait_sandbox_cards(page, ws, base)
        self.assertEqual(page.locator("h1").first.inner_text().strip(), "Sandboxes")

        live_n = 0
        for sess in session_rows:
            sid = str(sess["id"])
            run_id = str(sess.get("run_id") or "")
            task_id = str(sess.get("board_task_id") or "")
            self.assertTrue(run_id, f"session missing run_id: {sess}")
            self.assertTrue(task_id, f"session missing board_task_id: {sess}")
            sid8, run8, task8 = _short_id(sid), _short_id(run_id), _short_id(task_id)
            self.assertRegex(
                cards,
                rf"session\s+{re.escape(sid8)}",
                f"card missing session short id {sid8}: {cards[:1200]!r}",
            )
            self.assertRegex(
                cards,
                rf"run\s+{re.escape(run8)}",
                f"card missing run short id {run8}: {cards[:1200]!r}",
            )
            self.assertRegex(
                cards,
                rf"task\s+{re.escape(task8)}",
                f"card missing task short id {task8}: {cards[:1200]!r}",
            )
            if str(sess.get("status") or "") in _LIVE_SANDBOX_STATUSES:
                live_n += 1

        self.assertGreater(
            live_n,
            0,
            "fixture + tick must leave a live worker session so cards are "
            "status-only (no Kill): "
            f"{[(s.get('id'), s.get('status')) for s in session_rows]!r}",
        )
        self._assert_sandboxes_page_has_no_destroy(page, where="after fleet run")

        _goto_ws_page(page, base, "/sessions")
        after_h1 = page.locator("h1").first.inner_text().strip()
        self.assertEqual(
            after_h1,
            "Chat history",
            f"Chat /sessions heading must stay Chat history after a fleet run: "
            f"{after_h1!r}",
        )
        after = page.inner_text("body")
        for sess in session_rows:
            sid8 = _short_id(str(sess["id"]))
            self.assertIsNone(
                re.search(rf"session\s+{re.escape(sid8)}", after),
                "Chat /sessions must not render worker session cards",
            )

    def test_sandboxes_card_watch_opens_live_session(self) -> None:
        # catalog:e2e-sandboxes-card-watch-opens-live-session
        # Docs §6.4 / §6.5: Watch on a sandbox card opens /{ws}/sessions/{id}.
        context = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = context.new_page()
        page.set_default_timeout(20_000)
        try:
            ws, base, _seeded = self._start_live_sandbox_sessions(
                page, prefix="sbx-watch"
            )
            _cards, session_rows = self._wait_sandbox_cards(page, ws, base)
            want_ids = {str(s["id"]) for s in session_rows}
            watch = page.get_by_role("link", name="Watch")
            watch.first.wait_for(state="visible", timeout=15_000)
            watch.first.click()
            page.wait_for_url(
                re.compile(rf".*/{re.escape(ws)}/sessions/[0-9a-f-]+/?$"),
                timeout=15_000,
            )
            landed = re.search(
                rf"/{re.escape(ws)}/sessions/([0-9a-f-]+)/?",
                page.url,
            )
            self.assertIsNotNone(
                landed,
                f"Watch must open /{{ws}}/sessions/{{id}}: {page.url}",
            )
            self.assertIn(
                landed.group(1),
                want_ids,
                f"Watch landed on unknown session {landed.group(1)} "
                f"not in {want_ids}: {page.url}",
            )
            live_h1 = page.locator("h1").first.inner_text().strip()
            self.assertEqual(
                live_h1,
                "Live session",
                f"Watch must land on Live session: {live_h1!r} {page.url}",
            )
        finally:
            context.close()

    def _assert_sandboxes_page_has_no_destroy(self, page, *, where: str) -> None:
        h1 = page.locator("h1").first.inner_text().strip()
        self.assertEqual(h1, "Sandboxes", f"{where}: unexpected heading {h1!r}")
        for role in ("button", "link"):
            n = page.get_by_role(role, name=_SANDBOX_DESTROY_BTN, exact=True).count()
            self.assertEqual(
                n,
                0,
                f"{where}: sandboxes page must not expose a {role} named "
                f"Kill/Destroy (docs §6.6); found {n}",
            )
        for label in ("Kill", "Destroy", "Terminate", "Reap"):
            self.assertEqual(
                page.get_by_role("button", name=label, exact=True).count(),
                0,
                f"{where}: sandboxes page must not expose {label!r}",
            )

    def test_sessions_page_lists_chats_not_workers(self) -> None:
        # catalog:list-sessions-page-lists-chats-not-workers
        # Docs §6.3: /{ws}/sessions lists saved chats (GET conversations,
        # click → /chat/{id}), not worker fleet rows.
        # Own context: signup must not clobber the suite session / test_18 logout.
        context = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = context.new_page()
        page.set_default_timeout(20_000)
        try:
            self._assert_sessions_page_lists_chats_not_workers(page)
        finally:
            context.close()

    def _assert_sessions_page_lists_chats_not_workers(self, page) -> None:
        ws, base = signup_login(page, prefix="sess-chats")
        title = f"saved-chat-{uuid.uuid4().hex[:10]}"
        created = _json_ok(
            page.request.post(
                f"{UI}/api/workspaces/{ws}/chat/conversations",
                data=json.dumps({"title": title}),
                headers={"Content-Type": "application/json"},
            )
        )
        cid = str((created.get("conversation") or {}).get("id") or "")
        self.assertTrue(cid, f"POST /chat/conversations missing id: {created!r}"[:600])

        conv_gets: list[str] = []
        fleet_session_gets: list[str] = []
        fleet_worker_gets: list[str] = []

        def on_request(req) -> None:
            if req.method.upper() != "GET":
                return
            url = req.url
            if f"/api/workspaces/{ws}/chat/conversations" in url:
                conv_gets.append(url)
            if re.search(rf"/api/workspaces/{ws}/fleet/sessions(?:/|\?|$)", url):
                fleet_session_gets.append(url)
            if f"/api/workspaces/{ws}/fleet/workers" in url:
                fleet_worker_gets.append(url)

        page.on("request", on_request)
        try:
            _goto_ws_page(page, base, "/sessions")
            h1 = page.locator("h1").first.inner_text().strip()
            self.assertEqual(
                h1,
                "Chat history",
                f"/{ws}/sessions must be Chat history, not worker rows: {h1!r}",
            )
            self.assertRegex(
                page.url,
                rf"/{re.escape(ws)}/sessions/?$",
                f"chat history URL must be /{{ws}}/sessions, not live view: {page.url}",
            )
            page.get_by_role("heading", name=title, exact=True).wait_for(timeout=10_000)
            self.assertGreater(
                len(conv_gets),
                0,
                "Chat history must GET /chat/conversations",
            )
            self.assertEqual(
                fleet_session_gets,
                [],
                "Chat history must not GET /fleet/sessions "
                f"(worker list/detail): {fleet_session_gets!r}",
            )
            self.assertEqual(
                fleet_worker_gets,
                [],
                "Chat history must not GET /fleet/workers: "
                f"{fleet_worker_gets!r}",
            )
        finally:
            page.remove_listener("request", on_request)

        page.get_by_role("heading", name=title, exact=True).click()
        page.wait_for_url(
            re.compile(rf".*/{re.escape(ws)}/chat/{re.escape(cid)}/?$"),
            timeout=10_000,
        )
        self.assertRegex(
            page.url,
            rf"/{re.escape(ws)}/chat/{re.escape(cid)}/?$",
            f"row click must open /chat/{{id}}, not worker live: {page.url}",
        )
        self.assertNotRegex(
            page.url,
            rf"/{re.escape(ws)}/sessions/",
            f"row click must not open /sessions/{{id}}: {page.url}",
        )

    def test_sessions_hover_delete_removes_saved_chat(self) -> None:
        # catalog:e2e-sessions-hover-delete-removes-saved-chat
        # Docs §6.3: hover Delete on a saved chat drops the title and
        # GET /chat/conversations no longer lists that id.
        # Own context: signup must not clobber the suite session / test_18 logout.
        context = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = context.new_page()
        page.set_default_timeout(20_000)
        try:
            self._assert_sessions_hover_delete_removes_saved_chat(page)
        finally:
            context.close()

    def _assert_sessions_hover_delete_removes_saved_chat(self, page) -> None:
        ws, base = signup_login(page, prefix="sess-del")
        title = f"delete-chat-{uuid.uuid4().hex[:10]}"
        created = _json_ok(
            page.request.post(
                f"{UI}/api/workspaces/{ws}/chat/conversations",
                data=json.dumps({"title": title}),
                headers={"Content-Type": "application/json"},
            )
        )
        cid = str((created.get("conversation") or {}).get("id") or "")
        self.assertTrue(cid, f"POST /chat/conversations missing id: {created!r}"[:600])

        _goto_ws_page(page, base, "/sessions")
        heading = page.get_by_role("heading", name=title, exact=True)
        heading.wait_for(timeout=10_000)
        row = page.get_by_role("listitem").filter(has=heading)
        row.hover()
        row.get_by_role("button", name="Delete", exact=True).click()
        heading.wait_for(state="detached", timeout=10_000)
        self.assertNotIn(cid, _chat_conversation_ids(page, ws))

    def test_chat_send_empty_model_blocked(self) -> None:
        # catalog:list-chat-send-empty-model-blocked
        # Docs §6.2: new-chat Send with empty Ollama model must not POST /chat
        # or create a conversation; draft stays. Control: a listed tag POSTs.
        # Own context: signup must not clobber the suite session / test_18 logout.
        context = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = context.new_page()
        page.set_default_timeout(20_000)
        try:
            self._assert_chat_send_empty_model_blocked(page)
        finally:
            context.close()

    def _assert_chat_send_empty_model_blocked(self, page) -> None:
        ws, base = signup_login(page, prefix="chat-nomodel")
        host_name = f"chat-empty-model-{uuid.uuid4().hex[:8]}"
        # Blank listed model keeps Ollama available (Send enabled) while
        # selected_model.trim() is empty — the §6.2 case. A real tag is the
        # control so a no-op Send cannot pass as "blocked".
        ready_model = "llama3.2:latest"
        pair_and_heartbeat(
            page,
            ws,
            host_name,
            heartbeat_extra={
                "agents": [
                    {
                        "id": "ollama",
                        "name": "Ollama",
                        "installed": True,
                        "logged_in": True,
                        "models": ["", ready_model],
                    }
                ]
            },
        )

        before_ids = _chat_conversation_ids(page, ws)
        posted: list[str] = []

        def _gate_chat_posts(route) -> None:
            req = route.request
            if _is_chat_turn_post(req.method, req.url):
                posted.append(f"{req.method} {req.url}")
                route.abort()
                return
            route.continue_()

        page.route(_CHAT_TURN_POST_RE, _gate_chat_posts)
        try:
            page.goto(
                f"{base}/chat",
                wait_until="domcontentloaded",
                timeout=15_000,
            )
            _wait_dx_ready(page)
            wait_dx_ready(page)
            page.get_by_text("How can I help?").wait_for(timeout=15_000)

            box = page.locator("textarea")
            box.wait_for(state="visible", timeout=10_000)
            model_select = page.locator("select").filter(
                has=page.locator("option", has_text=ready_model)
            )
            model_select.wait_for(state="visible", timeout=15_000)
            model_select.select_option(value="")

            new_msg = f"empty-model-blocked-{uuid.uuid4().hex[:8]}"
            box.fill(new_msg)
            send = page.get_by_role("button", name="Send message")
            send.first.wait_for(state="visible", timeout=10_000)
            send.first.click()
            page.get_by_role("status").first.wait_for(state="visible", timeout=8_000)

            self.assertEqual(
                posted,
                [],
                "Send with empty model must not POST /chat, "
                f"/chat/stream, or /chat/conversations: {posted!r}",
            )
            self.assertEqual(
                box.input_value(),
                new_msg,
                "draft must stay (send returns before clearing it)",
            )
            self.assertEqual(
                _chat_conversation_ids(page, ws),
                before_ids,
                "empty-model Send must not create a conversation",
            )
            self.assertRegex(
                page.url.rstrip("/"),
                r"/chat$",
                f"must stay on New chat, not /chat/{{id}}: {page.url}",
            )

            # Control: a selected model must attempt POST (aborted — no LLM).
            model_select.select_option(value=ready_model)
            control_msg = f"empty-model-control-{uuid.uuid4().hex[:8]}"
            box.fill(control_msg)
            posted.clear()
            with page.expect_request(
                lambda r: _is_chat_turn_post(r.method, r.url),
                timeout=8_000,
            ):
                send.first.click()
            self.assertTrue(
                posted,
                "control: Send with a selected model must POST /chat so the "
                f"empty-model block is not a no-op Send. url={page.url!r}",
            )
        finally:
            page.unroute(_CHAT_TURN_POST_RE, _gate_chat_posts)

    def test_chat_openai_coming_soon_snaps_back(self) -> None:
        # catalog:list-chat-openai-coming-soon-snaps-back
        # Docs §6.2: new-chat pick of coming-soon OpenAI toasts and snaps
        # to the first available provider; must not POST provider=openai.
        # Own context: signup must not clobber the suite session / test_18 logout.
        context = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = context.new_page()
        page.set_default_timeout(20_000)
        try:
            self._assert_chat_openai_coming_soon_snaps_back(page)
        finally:
            context.close()

    def _assert_chat_openai_coming_soon_snaps_back(self, page) -> None:
        ws, base = signup_login(page, prefix="chat-oai")
        host_name = f"chat-openai-soon-{uuid.uuid4().hex[:8]}"
        created, paired, _hb = pair_and_heartbeat(
            page,
            ws,
            host_name,
            heartbeat_extra={
                "agents": [
                    {
                        "id": "ollama",
                        "name": "Ollama",
                        "installed": True,
                        "logged_in": True,
                        "models": ["llama3.2:latest"],
                    }
                ]
            },
        )
        host = created.get("host") if isinstance(created.get("host"), dict) else {}
        host_id = str(host.get("id") or created.get("id") or paired.get("id") or "")
        self.assertTrue(host_id, f"create host missing id: {created!r}"[:400])

        status = _json_ok(
            page.request.get(
                f"{UI}/api/workspaces/{ws}/chat/status?runner_id={host_id}"
            )
        )
        first_available = _first_live_chat_provider(status)
        self.assertEqual(
            first_available,
            "ollama",
            "paired Ollama must be the first available provider so snap-back "
            f"is unambiguous: {status!r}"[:800],
        )

        openai_posts: list[str] = []

        def _gate_chat_posts(route) -> None:
            req = route.request
            if _is_chat_turn_post(req.method, req.url):
                provider = _json_provider(req.post_data)
                if provider.lower() == "openai":
                    openai_posts.append(f"{req.method} {req.url} provider={provider!r}")
                route.abort()
                return
            route.continue_()

        page.route(_CHAT_TURN_POST_RE, _gate_chat_posts)
        try:
            page.goto(
                f"{base}/chat",
                wait_until="domcontentloaded",
                timeout=15_000,
            )
            _wait_dx_ready(page)
            wait_dx_ready(page)
            page.get_by_text("How can I help?").wait_for(timeout=15_000)

            runner = page.locator("select").filter(
                has=page.locator(f"option[value='{host_id}']")
            )
            runner.wait_for(state="visible", timeout=15_000)
            if runner.input_value() != host_id:
                runner.select_option(value=host_id)

            provider = page.locator("select").filter(
                has=page.locator("option[value='openai']")
            )
            provider.wait_for(state="visible", timeout=15_000)
            page.locator("select").filter(
                has=page.locator(f"option[value='{first_available}']")
            ).wait_for(state="visible", timeout=15_000)

            # Native select_option waits for the value to stick; coming-soon
            # snap-back would look like a failed select. Dispatch change instead.
            provider.evaluate(
                """(s) => {
                    const opt = [...s.options].find((o) => o.value === "openai");
                    if (!opt) throw new Error("openai option missing");
                    if (opt.disabled) {
                        throw new Error("openai option disabled (must be pickable)");
                    }
                    s.value = "openai";
                    s.dispatchEvent(new Event("input", { bubbles: true }));
                    s.dispatchEvent(new Event("change", { bubbles: true }));
                }"""
            )

            page.get_by_role("status").filter(
                has_text=re.compile(r"coming soon", re.I)
            ).first.wait_for(state="visible", timeout=8_000)
            page.wait_for_function(
                """(expected) => {
                    const s = [...document.querySelectorAll("select")].find((el) =>
                        [...el.options].some((o) => o.value === "openai")
                    );
                    return !!(s && s.value === expected);
                }""",
                arg=first_available,
                timeout=8_000,
            )
            self.assertEqual(
                provider.input_value(),
                first_available,
                "coming-soon OpenAI pick must snap the provider select to "
                f"{first_available!r}",
            )
            self.assertEqual(
                openai_posts,
                [],
                "picking coming-soon OpenAI must not POST provider=openai: "
                f"{openai_posts!r}",
            )
        finally:
            page.unroute(_CHAT_TURN_POST_RE, _gate_chat_posts)

    def test_18_logout_returns_to_login(self) -> None:
        # Own context: user-menu Log out POSTs /logout and replaces /login.
        # Same-context /dashboard bounce must stay on /login (session dead).
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            _ws, base = signup_login(page, prefix="logout")
            page.locator("button[aria-haspopup='menu']").click()
            with page.expect_response(
                lambda r: "/api/auth/logout" in r.url
                and r.request.method == "POST",
                timeout=15_000,
            ) as logout_info:
                page.get_by_role("button", name="Log out", exact=True).click()
            logout_res = logout_info.value
            self.assertTrue(
                logout_res.ok,
                f"Log out must POST /api/auth/logout 2xx: {logout_res.status}",
            )
            page.wait_for_url("**/login", timeout=15_000)
            self.assertIn("/login", page.url)
            page.goto(f"{base}/dashboard", wait_until="domcontentloaded")
            page.wait_for_url("**/login", timeout=15_000)
            self.assertIn("/login", page.url)
        finally:
            ctx.close()

    def test_logout_failed_stays_signed_in(self) -> None:
        # Own context: 500 on POST /logout stays off /login and toasts.
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)

        def _fail_logout(route) -> None:
            if route.request.method.upper() == "POST":
                route.fulfill(
                    status=500,
                    content_type="application/json",
                    body='{"error":"logout failed"}',
                )
                return
            route.continue_()

        try:
            signup_login(page, prefix="logout-fail")
            page.route("**/api/auth/logout", _fail_logout)
            try:
                page.locator("button[aria-haspopup='menu']").click()
                page.get_by_role("button", name="Log out", exact=True).click()
                page.get_by_role("status").filter(
                    has_text=re.compile(r"Log out failed", re.I)
                ).first.wait_for(state="visible", timeout=15_000)
                self.assertNotIn("/login", page.url)
            finally:
                page.unroute("**/api/auth/logout", _fail_logout)
        finally:
            ctx.close()


if __name__ == "__main__":
    unittest.main(verbosity=2)
