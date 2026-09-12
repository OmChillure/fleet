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
import threading
import time
import unittest
import uuid
from pathlib import Path
from urllib.parse import urlparse

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runners_e2e_helpers import (  # noqa: E402
    assert_no_control_plane_card,
    click_add_worker,
    create_host,
    goto_self_hosted,
    heartbeat,
    list_hosts,
    open_host_panel,
    pair_and_heartbeat,
    pair_host,
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


def _url_listen(url: str, default_port: int) -> tuple[str, int]:
    parsed = urlparse(url)
    host = parsed.hostname or "127.0.0.1"
    if host in {"localhost", "::1"}:
        host = "127.0.0.1"
    port = parsed.port or default_port
    return host, port


UI_LISTEN = _url_listen(UI, 3030)
API_LISTEN = _url_listen(API, 8080)


def _require_stack() -> None:
    try:
        require_this_tree(REPO, API)
    except ForeignStackError as e:
        raise RuntimeError(str(e)) from e
    host, port = UI_LISTEN
    if not _listening(host, port):
        raise RuntimeError(
            f"UI {UI} is not listening. Start this tree: ./scripts/human-test.sh launch"
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
# First-send creates POST /chat/conversations then POST /chat or /chat/stream.
_CHAT_TURN_POST_RE = re.compile(
    r"/api/workspaces/[^/]+/chat(?:/stream|/conversations)?(?:\?|$)",
    re.I,
)
# Browser or proxy must never phone OpenAI for the coming-soon pick (docs §6.2).
_OPENAI_URL_RE = re.compile(
    r"https?://(?:[^/\s]*\.)?openai\.com(?:[:/]|$)",
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


def _is_chat_turn_post(method: str, url: str) -> bool:
    """POST that would create a conversation or an empty-model turn."""
    if method.upper() != "POST":
        return False
    return _CHAT_TURN_POST_RE.search(url) is not None


def _is_openai_url(url: str) -> bool:
    return bool(_OPENAI_URL_RE.search(url or ""))


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
    """Wait out a real dx compile splash; a leftover toast is not a block."""
    wait_dx_ready(page, timeout_ms=20_000)


_LIVE_SANDBOX_STATUSES = frozenset({"spawning", "healthy", "stuck", "killing"})
# Docs §6.6 / §17: cards are status-only. Cancel the run; do not kill here.
_SANDBOX_DESTROY_BTN = re.compile(
    r"\b(kill|destroy|terminate|reap|delete sandbox)\b",
    re.I,
)
_SANDBOX_KILL_POST = re.compile(
    r"/fleet/sessions/[^/]+/kill(?:/|$|\?)",
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


def _last_exec_copy(page) -> str:
    copies = page.evaluate("() => window.__execCopies || []")
    if not copies:
        raise AssertionError(
            "HTTP LAN fallback never called document.execCommand('copy')"
        )
    last = copies[-1]
    if not isinstance(last, str):
        raise AssertionError(f"execCommand copy payload is not text: {last!r}")
    return last


def _exec_copy_count(page) -> int:
    n = page.evaluate("() => (window.__execCopies || []).length")
    return int(n or 0)


def _clipboard_getter_hits(page) -> int:
    n = page.evaluate("() => Number(window.__clipboardGets || 0)")
    return int(n)


def _copy_button(page, pre_needle: str, *, last: bool = False):
    loc = page.locator("pre").filter(has_text=pre_needle)
    pre = loc.last if last else loc.first
    return pre.locator("xpath=..").get_by_role("button", name="Copy")


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


def _dismiss_status_toasts(page) -> None:
    """Clear leftover status toasts (they stay 4s and match later wait_for)."""
    for _ in range(8):
        btn = page.get_by_role("button", name="Dismiss")
        if btn.count() == 0 or not btn.first.is_visible():
            return
        try:
            btn.first.click(force=True)
        except Exception:
            return
        page.wait_for_timeout(50)


def _open_pair_panel(page, workspace_url: str) -> None:
    add = page.get_by_role("button", name="Add worker")
    deadline = time.time() + 180
    last_body = ""
    last_err: Exception | None = None
    while time.time() < deadline:
        if not _listening(*UI_LISTEN) or not _listening(*API_LISTEN):
            raise AssertionError(
                "stack died while opening pair panel "
                f"(ui={UI} up={_listening(*UI_LISTEN)} "
                f"api={API} up={_listening(*API_LISTEN)})"
            )
        try:
            if "chrome-error" in (page.url or "") or not add.count():
                goto_self_hosted(page, workspace_url)
            wait_dx_ready(page, timeout_ms=3_000)
            last_body = page.inner_text("body")
            if add.count() and add.first.is_visible():
                click_add_worker(page)
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
        cls._progress_current = "setUpClass"
        cls._progress_started = time.time()
        stop = {"on": False}

        def _heartbeat() -> None:
            while not stop["on"]:
                time.sleep(30)
                if stop["on"]:
                    return
                name = getattr(cls, "_progress_current", "?")
                started = getattr(cls, "_progress_started", time.time())
                print(
                    f"PROGRESS still {name} ({int(time.time() - started)}s)",
                    flush=True,
                )

        t = threading.Thread(target=_heartbeat, name="e2e-progress", daemon=True)
        t.start()
        cls._progress_stop = stop
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

    def _assert_lands_on_self_hosted(self, page, workspace_url: str, path: str) -> None:
        """/{ws}/runners and /{ws}/workers must land on Self-hosted, not a blank accordion."""
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
                page.wait_for_url("**/runners/self-hosted", timeout=15_000)
                page.locator("main h1").first.wait_for(state="visible", timeout=10_000)
                last_err = None
                break
            except Exception as e:  # noqa: BLE001 — dx mid-rebuild; retry
                last_err = e
                time.sleep(2)
        if last_err is not None:
            raise AssertionError(
                f"{path} did not land on Self-hosted (blank accordion?): {last_err}"
            ) from last_err
        self.assertRegex(
            page.url,
            r"/[0-9a-f-]{36}/runners/self-hosted(?:[?#].*)?$",
            f"{path} must replace to /{{ws}}/runners/self-hosted, got {page.url}",
        )
        path_only = page.url.split("?", 1)[0].split("#", 1)[0].rstrip("/")
        self.assertTrue(
            path_only.endswith("/runners/self-hosted"),
            f"{path} stayed on a blank redirect route: {page.url}",
        )
        self.assertFalse(
            path_only.endswith("/workers") or path_only.endswith("/runners"),
            f"{path} must not remain on the alias/index: {page.url}",
        )
        heading = page.locator("main h1").first.inner_text().strip()
        self.assertEqual(
            heading,
            "Self-hosted",
            f"{path} must land on Self-hosted heading, not a blank accordion: {heading!r}",
        )
        main = page.locator("main")
        self.assertGreater(
            main.get_by_role("button", name="Add worker").count(),
            0,
            f"{path}: Self-hosted page missing Add worker (blank accordion?)",
        )
        body = main.inner_text()
        self.assertIn("Pair a machine as a self-hosted worker", body)
        self.assertIn("this process", body)
        # Runners accordion must be expanded with children — not a blank accordion.
        nav = page.locator("aside nav")
        self.assertTrue(
            nav.get_by_text("Runners", exact=True).first.is_visible(),
            f"{path}: sidebar missing Runners",
        )
        self.assertTrue(
            nav.get_by_role("link", name="Self-hosted", exact=True).first.is_visible(),
            f"{path}: Runners accordion must show Self-hosted, not stay blank",
        )
        self.assertTrue(
            nav.get_by_role("link", name="Cloud", exact=True).first.is_visible(),
            f"{path}: Runners accordion missing Cloud",
        )
        self.assertTrue(
            nav.get_by_role("link", name="Partners", exact=True).first.is_visible(),
            f"{path}: Runners accordion missing Partners",
        )
        self.assertEqual(
            nav.get_by_role("link", name="Workers", exact=True).count(),
            0,
            "Workers must not remain a sidebar child",
        )

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
        stop = getattr(cls, "_progress_stop", None)
        if stop is not None:
            stop["on"] = True
        cls._context.close()
        cls._browser.close()
        cls._pw.stop()

    def setUp(self) -> None:
        type(self)._progress_current = self.id()
        type(self)._progress_started = time.time()
        print(f"PROGRESS start {self.id()}", flush=True)

    def tearDown(self) -> None:
        started = getattr(type(self), "_progress_started", time.time())
        print(
            f"PROGRESS done {self.id()} ({int(time.time() - started)}s)",
            flush=True,
        )

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

        page.get_by_role("link", name="Cases", exact=True).click()
        page.wait_for_url("**/cases", timeout=10_000)
        page.wait_for_selector("h1")
        self.assertEqual(page.locator("h1").first.inner_text().strip(), "Cases")
        self.assertTrue(
            page.get_by_role("button", name="Create case").count() > 0,
            "Cases page missing Create case",
        )

        page.get_by_role("link", name="Inbox", exact=True).click()
        page.wait_for_url("**/inbox", timeout=10_000)
        page.wait_for_selector("h1")
        self.assertEqual(page.locator("h1").first.inner_text().strip(), "Inbox")
        self.assertTrue(
            page.get_by_role("button", name="Start monitor").count() > 0,
            "Inbox page missing Start monitor",
        )
        self.assertTrue(
            page.get_by_role("button", name="Refresh now").count() > 0,
            "Inbox page missing Refresh now",
        )

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

        # Workspace accordion starts collapsed. The workspace switcher is also
        # a button named Workspace; the accordion is the one with the ▸/▾ chevron.
        integ = page.get_by_role("link", name="Integrations", exact=True)
        if not integ.is_visible():
            page.get_by_role("button", name="Workspace ▸").click()
            integ.wait_for(state="visible", timeout=10_000)
        integ.click()
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
        page.locator("main h1").first.wait_for(state="visible", timeout=10_000)
        runners_h1 = page.locator("main h1").first.inner_text().strip()
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
        deadline = time.time() + 15
        runners_body = page.inner_text("body")
        while time.time() < deadline and "Loading…" in runners_body:
            page.wait_for_timeout(200)
            runners_body = page.inner_text("body")
        self.assertIn("Pair a machine as a self-hosted worker", runners_body)
        # Empty copy only when this workspace has no rows. A paired
        # this-machine box (the plane laptop) is listed as an extra.
        if "this machine" not in runners_body.lower():
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
        click_add_worker(page)
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
        page.get_by_role("button", name="Done", exact=True).click()
        page.get_by_role("heading", name="Pair this worker").wait_for(state="hidden")
        self.assertNotIn(token, page.inner_text("body"))
        page.reload(wait_until="networkidle")
        page.wait_for_selector("h1")
        self.assertNotIn(token, page.inner_text("body"))
        pending_card = open_host_panel(page, "Waiting to pair", timeout_ms=10_000)
        pending_text = pending_card.first.inner_text()
        card = page.inner_text("body")
        self.assertTrue(
            pending_card.get_by_role("button", name="Remove worker").first.is_visible()
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
        self.assertIn("SANDBOXING", pending_text.upper())
        self.assertNotIn("not implemented yet", card.lower())
        self.assertIn("Runtime:", card)
        self.assertIn("behavior", card.lower())
        self.assertIn("Auto-update", card)
        self.assertRegex(card, r"id [0-9a-f]{8}")
        self.assertRegex(card.lower(), r"jobs\s+0\s*/\s*4")
        self.assertNotIn("Advertised", pending_text)

    def test_15_cancel_discards_pending_worker(self) -> None:
        base = getattr(type(self), "workspace_url", None)
        self.assertIsNotNone(base, "signup/login must run first")
        page = self.page
        page.goto(f"{base}/runners/self-hosted", wait_until="networkidle")
        page.wait_for_selector("h1")
        before = page.locator("article").count()
        click_add_worker(page)
        page.get_by_role("heading", name="Pair this worker").wait_for(timeout=15_000)
        token = page.locator("pre").filter(has_text="flp_").last.inner_text().strip()
        with page.expect_response(
            lambda r: r.request.method.upper() == "DELETE"
            and "/worker-hosts/" in r.url,
            timeout=15_000,
        ) as revoke:
            page.get_by_role("button", name="Cancel").click()
        self.assertTrue(
            revoke.value.ok,
            f"Cancel DELETE leftover: {revoke.value.status}",
        )
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

    def test_16b_fleet_header_says_paired_hosts(self) -> None:
        """Fleet chrome says jobs run on paired hosts; the plane is not a runner."""
        base = getattr(type(self), "workspace_url", None)
        self.assertIsNotNone(base, "signup/login must run first")
        ws = base.rstrip("/").rsplit("/", 1)[-1]
        # Cookie is first-party on the UI origin (dx proxy), not FLEET_API_URL.
        res = self.page.request.get(f"{UI}/api/workspaces/{ws}/fleet")
        self.assertEqual(res.status, 200, res.text()[:400])

        page = self.page
        page.goto(f"{base}/fleet", wait_until="networkidle")
        page.wait_for_selector("h1")
        self.assertEqual(page.inner_text("h1").strip(), "Fleet")
        body = page.inner_text("body")
        self.assertIn("paired hosts", body.lower())
        self.assertIn("plane is not a runner", body.lower())

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

    def _assert_fleet_run_until_done_caps_24_ticks(self, page) -> None:
        page_rs = (
            REPO / "crates" / "fleet-ui" / "src" / "pages" / "fleet" / "page.rs"
        ).read_text()
        self.assertIn(
            "let max = 24u32;",
            page_rs,
            "docs §6.5: Run until done must cap at 24 ticks",
        )
        self.assertIn(
            "TimeoutFuture::new(400)",
            page_rs,
            "docs §6.5: ticks must be 400ms apart",
        )
        self.assertIn(
            "Stopped after {max} ticks",
            page_rs,
            "docs §6.5: hitting the cap must stop and say so (not loop forever)",
        )

        ws, base = signup_login(page, prefix="fleet-cap24")
        _goto_ws_page(page, base, "/fleet")
        self.assertEqual(page.locator("h1").first.inner_text().strip(), "Fleet")
        until = page.get_by_role("button", name="Run until done")
        self.assertGreater(until.count(), 0, "Fleet page missing Run until done")

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

            _click_enabled(page, "Run until done")
            page.get_by_text(
                re.compile(r"Orchestrating… tick \d+/24")
            ).wait_for(timeout=10_000)
            # 24 × 400ms ≈ 9.6s plus fetch; leave room for a slow dx box.
            page.get_by_text("Stopped after 24 ticks").wait_for(timeout=45_000)

            body = page.inner_text("body")
            self.assertIn(
                "Stopped after 24 ticks — press Run tick or refresh",
                body,
            )
            self.assertNotRegex(
                body,
                r"Run finished after \d+ tick",
                "must stop on the 24-tick cap, not because the run went "
                f"terminal: {body[:1200]!r}",
            )

            self.assertEqual(
                len(tick_at),
                _RUN_UNTIL_DONE_MAX_TICKS,
                "Run until done must POST /fleet/tick exactly 24 times when "
                f"the run stays active, got {len(tick_at)}",
            )
            min_gap = (_RUN_UNTIL_DONE_GAP_MS / 1000) * 0.75
            gaps = [tick_at[i + 1] - tick_at[i] for i in range(len(tick_at) - 1)]
            self.assertTrue(
                gaps and all(g >= min_gap for g in gaps),
                "docs §6.5: ticks must be 400ms apart (not a tight spin): "
                f"gaps={gaps!r}",
            )
            span = tick_at[-1] - tick_at[0]
            self.assertGreaterEqual(
                span,
                (_RUN_UNTIL_DONE_MAX_TICKS - 1) * min_gap,
                f"24 ticks must span ~23×400ms, got {span:.3f}s",
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

            page.wait_for_timeout(1_500)
            self.assertEqual(
                len(tick_at),
                _RUN_UNTIL_DONE_MAX_TICKS,
                "must not POST a 25th tick after the cap: "
                f"got {len(tick_at)}",
            )
            until = page.get_by_role("button", name="Run until done")
            self.assertTrue(
                until.first.is_enabled(),
                "Run until done must re-enable after the 24-tick stop",
            )
        finally:
            page.unroute(_FLEET_TICK_POST, _fulfill_tick)

    def test_17_apply_changes_when_dirty(self) -> None:
        base = getattr(type(self), "workspace_url", None)
        self.assertIsNotNone(base, "signup/login must run first")
        page = self.page
        page.goto(f"{base}/runners/self-hosted", wait_until="networkidle")
        page.wait_for_selector("h1")
        auto = page.locator("label").filter(has_text="Auto-update")
        if auto.count() == 0:
            pending = page.get_by_role("button").filter(has_text="Waiting to pair")
            if pending.count() == 0:
                click_add_worker(page)
                page.get_by_role("heading", name="Pair this worker").wait_for(timeout=15_000)
                page.get_by_role("button", name="Done", exact=True).click()
                page.get_by_role("heading", name="Pair this worker").wait_for(state="hidden")
            open_host_panel(page, "Waiting to pair")
            auto = page.locator("label").filter(has_text="Auto-update")
        apply = page.get_by_role("button", name="Apply", exact=True)
        self.assertEqual(apply.count(), 0, "Apply must stay hidden until a setting changes")
        self.assertGreater(auto.count(), 0, "paired or pending card needs Auto-update")
        auto.last.locator("input").click(force=True)
        page.get_by_role("button", name="Apply", exact=True).wait_for(timeout=8_000)
        self.assertTrue(page.get_by_role("button", name="Apply", exact=True).first.is_visible())

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
        # The workspace switcher is also a button named Workspace; the accordion
        # is the one with the ▸/▾ chevron.
        integ = nav.get_by_role("link", name="Integrations", exact=True)
        if not integ.first.is_visible():
            page.get_by_role("button", name="Workspace ▸").click()
            integ.first.wait_for(state="visible", timeout=10_000)
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
        # catalog:e2e-route-nav-redirects — /{ws}/workers → Self-hosted heading.
        _ws, base = _ensure_workspace(self.page)
        self._assert_lands_on_self_hosted(self.page, base, "/workers")

    def test_09_runners_redirects_to_self_hosted(self) -> None:
        # catalog:e2e-route-nav-redirects — /{ws}/runners and /{ws}/workers
        # land on Self-hosted heading, not a blank accordion.
        _ws, base = _ensure_workspace(self.page)
        self._assert_lands_on_self_hosted(self.page, base, "/runners")
        self._assert_lands_on_self_hosted(self.page, base, "/workers")

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

    def _assert_cloud_coming_soon_cards(self, page, where: str) -> None:
        main = page.locator("main")
        cards = main.locator("article")
        self.assertEqual(
            cards.count(),
            4,
            f"{where}: cloud page must render four Coming soon cards",
        )
        for label in ("AWS", "GCP", "Azure"):
            card = cards.filter(has_text=label)
            self.assertEqual(
                card.count(),
                1,
                f"{where}: expected one {label} card",
            )
            self.assertIn(
                "coming soon",
                card.first.inner_text().lower(),
                f"{where}: {label} must show Coming soon",
            )
        fleet = cards.filter(has_text="Fleet Cloud")
        self.assertEqual(fleet.count(), 1, f"{where}: expected Fleet Cloud card")
        self.assertGreater(
            main.get_by_role("button", name=re.compile(r"attach fleet cloud", re.I)).count(),
            0,
            f"{where}: Fleet Cloud must expose Attach",
        )
        for label in ("Spawn", "Lend", "Add worker", "Enqueue", "Pair"):
            self.assertEqual(
                page.get_by_role("button", name=label).count(),
                0,
                f"{where}: must not expose {label!r}",
            )
        body = main.inner_text().lower()
        for needle in ("spawn", "lend", "enqueue", "add worker"):
            self.assertNotIn(
                needle,
                body,
                f"{where}: must not offer {needle}",
            )

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
        self.assertIn("coming soon", body.lower(), "BYO cloud cards stay Coming soon")
        self.assertIn("attach", body.lower(), "Fleet Cloud must offer Attach")
        self._assert_cloud_coming_soon_cards(page, "cloud")
        self._assert_no_lend_enablement(page, "cloud")

    def test_12_partners_page_trust_disclaimer(self) -> None:
        page = self.page
        _ws, base = _ensure_workspace(page)
        page.goto(f"{base}/runners/partners", wait_until="networkidle")
        page.wait_for_selector("h1")
        self.assertRegex(page.url, r"/[0-9a-f-]{36}/runners/partners(?:[?#].*)?$")
        h1 = page.locator("h1").first.inner_text().strip()
        self.assertIn("Partners", h1, f"unexpected partners heading {h1!r}")
        main = page.locator("main")
        body = page.inner_text("body")
        body_l = body.lower()
        self.assertIn("trust", body_l, "partners page missing trust copy")
        self.assertIn("disclaimer", body_l, "partners page missing disclaimer copy")
        self.assertIn("lend", body_l, "partners page missing lend")
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
        page = self.page
        _ws, base = _ensure_workspace(page)
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
                if path == "/runners/cloud":
                    self._assert_cloud_coming_soon_cards(page, path)
                    for label in ("Spawn", "Lend"):
                        self.assertEqual(
                            page.get_by_role("button", name=label).count(),
                            0,
                            f"{path} must not expose {label!r}",
                        )
                if path == "/runners/partners":
                    main = page.locator("main")
                    body_l = main.inner_text().lower()
                    self.assertIn(
                        "trust",
                        body_l,
                        f"{path}: partners page must show trust copy",
                    )
                    self.assertIn(
                        "disclaimer",
                        body_l,
                        f"{path}: partners page must show the disclaimer",
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
        card = open_host_panel(page, name)
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

        card = open_host_panel(page, paired_name, timeout_ms=15_000)
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
        restored = page.get_by_role("button").filter(has_text=paired_name)
        restored.first.wait_for(timeout=10_000)
        self.assertTrue(restored.first.is_visible(), "Show removed must reveal the paired box")
        restored_text = restored.first.inner_text()
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
        card = open_host_panel(page, name, timeout_ms=30_000).first
        self.assertNotIn("Control plane", card.inner_text())
        self.assertEqual(
            card.get_by_text("pending apply", exact=True).count(),
            0,
            "fresh pair must not show pending apply",
        )

        auto = card.locator("label").filter(has_text="Auto-update").locator("input")
        self.assertGreater(auto.count(), 0, "paired card needs Auto-update")
        apply = card.get_by_role("button", name="Apply", exact=True)
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
            permissions=["clipboard-read", "clipboard-write"],
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

            _open_pair_panel(page, f"{UI}/{ws}")

            unix = page.locator("pre").filter(has_text="install-worker.sh").inner_text()
            win = page.locator("pre").filter(has_text="install-worker.ps1").inner_text()
            token = page.locator("pre").filter(has_text="flp_").last.inner_text().strip()
            self.assertIn("FLEET_PAIR_TOKEN=", unix, unix)
            self.assertIn("install-worker.sh", unix)
            self.assertIn("FLEET_PAIR_TOKEN=", win, win)
            self.assertIn("install-worker.ps1", win)
            self.assertTrue(token.startswith("flp_"), token)
            self.assertIn(token, unix)
            self.assertIn(token, win)

            unix_btn = _pair_copy_button(page, "install-worker.sh")
            win_btn = _pair_copy_button(page, "install-worker.ps1")
            token_btn = _pair_copy_button(page, "flp_", token_only=True)
            self.assertTrue(unix_btn.is_visible(), "Unix Copy missing")
            self.assertTrue(win_btn.is_visible(), "Windows Copy missing")
            self.assertTrue(token_btn.is_visible(), "token Copy missing")

            page.evaluate(_INSTALL_EXEC_SPY)
            self.assertTrue(
                page.evaluate(_STUB_CLIPBOARD_HTTP_LAN),
                "must hide navigator.clipboard to exercise HTTP LAN fallback",
            )
            page_errors.clear()

            def assert_lan_copy(btn, toast: str, *needles: str, exclude: str | None = None) -> str:
                before = _exec_copy_count(page)
                btn.click(force=True)
                page.get_by_role("status").filter(has_text=toast).first.wait_for(
                    timeout=5_000
                )
                failed = page.get_by_role("status").filter(has_text="Copy failed.")
                self.assertEqual(
                    failed.count(),
                    0,
                    "HTTP LAN copy must not toast failure when execCommand wrote",
                )
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
                fb = _last_exec_copy(page)
                for needle in needles:
                    self.assertIn(needle, fb)
                if exclude is not None:
                    self.assertNotIn(exclude, fb)
                return fb

            unix_fb = assert_lan_copy(
                unix_btn,
                "Command copied.",
                "install-worker.sh",
                "FLEET_PAIR_TOKEN=",
                token,
            )
            self.assertEqual(
                unix_fb.strip(),
                unix.strip(),
                "Unix Copy must place the shown install-worker.sh command",
            )

            win_fb = assert_lan_copy(
                win_btn,
                "Command copied.",
                "install-worker.ps1",
                "FLEET_PAIR_TOKEN=",
                token,
                exclude="install-worker.sh",
            )
            self.assertEqual(
                win_fb.strip(),
                win.strip(),
                "Windows Copy must place the shown install-worker.ps1 command",
            )

            tok_fb = assert_lan_copy(
                token_btn,
                "Token copied.",
                token,
                exclude="install-worker",
            )
            self.assertEqual(
                tok_fb.strip(),
                token,
                "token Copy must place only the shown flp_ token",
            )

            # execCommand already wrote the system clipboard; restore the API
            # on this same page (no reload — dx rebuild overlay would flake).
            self.assertTrue(
                page.evaluate(_RESTORE_CLIPBOARD_API),
                "must restore navigator.clipboard to read what fallback wrote",
            )
            tok_clip = _poll_clipboard(page, token, exclude="install-worker")
            self.assertIn("flp_", tok_clip)
            self.assertIn(token, tok_clip)

            # Secure-context still uses execCommand first. Clipboard-API-only
            # would write via writeText and leave __execCopies unchanged.
            def assert_secure_copy(
                btn, toast: str, *needles: str, exclude: str | None = None
            ) -> str:
                # LAN "Token copied." stays 4s. A leftover status would make
                # wait_for pass even when this click never ran execCommand.
                _dismiss_status_toasts(page)
                leftover = page.get_by_role("status").filter(has_text=toast)
                if leftover.count():
                    leftover.first.wait_for(state="hidden", timeout=6_000)
                before = _exec_copy_count(page)
                after = before
                for _attempt in range(3):
                    btn.scroll_into_view_if_needed()
                    btn.click(force=True)
                    deadline = time.time() + 2
                    while time.time() < deadline:
                        after = _exec_copy_count(page)
                        if after > before:
                            break
                        time.sleep(0.05)
                    if after > before:
                        break
                    try:
                        btn.evaluate("el => el.click()")
                    except Exception:
                        pass
                page.get_by_role("status").filter(has_text=toast).first.wait_for(
                    timeout=5_000
                )
                failed = page.get_by_role("status").filter(has_text="Copy failed.")
                self.assertEqual(
                    failed.count(),
                    0,
                    "Copy must not toast failure when execCommand wrote",
                )
                self.assertGreater(
                    after,
                    before,
                    "Copy must still use execCommand when navigator.clipboard exists",
                )
                clip = _poll_clipboard(page, needles[0], exclude=exclude)
                for needle in needles:
                    self.assertIn(needle, clip)
                if exclude is not None:
                    self.assertNotIn(exclude, clip)
                return clip

            unix_clip = assert_secure_copy(
                unix_btn,
                "Command copied.",
                "install-worker.sh",
                "FLEET_PAIR_TOKEN=",
                token,
            )
            self.assertEqual(
                unix_clip.strip(),
                unix.strip(),
                "Unix Copy must place the shown install-worker.sh command",
            )

            win_clip = assert_secure_copy(
                win_btn,
                "Command copied.",
                "install-worker.ps1",
                "FLEET_PAIR_TOKEN=",
                token,
                exclude="install-worker.sh",
            )
            self.assertEqual(
                win_clip.strip(),
                win.strip(),
                "Windows Copy must place the shown install-worker.ps1 command",
            )

            tok_clip2 = assert_secure_copy(
                token_btn,
                "Token copied.",
                token,
                exclude="install-worker",
            )
            self.assertEqual(
                tok_clip2.strip(),
                token,
                "token Copy must place only the shown flp_ token",
            )
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
        # Rename a pending (never-paired) card. A live cluster host heartbeats
        # its hostname back over a PATCH name.
        row = page.get_by_role("button").filter(has_text="Waiting to pair")
        if row.count() == 0:
            click_add_worker(page)
            page.get_by_role("heading", name="Pair this worker").wait_for(timeout=15_000)
            page.get_by_role("button", name="Done", exact=True).click()
            page.get_by_role("heading", name="Pair this worker").wait_for(state="hidden")
        card = open_host_panel(page, "Waiting to pair", timeout_ms=10_000).first
        assert_no_control_plane_card(page)

        card.get_by_title("Rename").click(force=True)
        inp = page.locator("input[autofocus]")
        inp.wait_for(timeout=5_000)
        saved = f"e2e-rename-{uuid.uuid4().hex[:8]}"
        inp.fill(saved)
        inp.press("Enter")
        page.get_by_text(saved, exact=True).first.wait_for(timeout=15_000)

        page.reload(wait_until="networkidle")
        page.wait_for_selector("h1")
        self.assertTrue(
            page.get_by_text(saved, exact=True).first.is_visible(),
            "Enter must PATCH the name so it sticks after reload",
        )

        open_host_panel(page, saved)
        page.get_by_title("Rename").click(force=True)
        inp = page.locator("input[autofocus]")
        inp.wait_for(timeout=5_000)
        cancelled = f"e2e-esc-{uuid.uuid4().hex[:8]}"
        inp.fill(cancelled)
        inp.press("Escape")
        page.get_by_text(saved, exact=True).first.wait_for(timeout=5_000)
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

        pending = open_host_panel(page, name, timeout_ms=15_000)
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
        live = open_host_panel(page, name, timeout_ms=15_000)
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

        card = open_host_panel(page, name, timeout_ms=15_000)
        self.assertNotIn("Control plane", card.first.inner_text())
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

        card = open_host_panel(page, name, timeout_ms=10_000).first
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
            page.get_by_role("button").filter(has_text=name).count(),
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
        try:
            card = open_host_panel(page, name, timeout_ms=20_000).first
        except Exception:
            page.wait_for_timeout(1500)
            page.reload(wait_until="domcontentloaded")
            page.get_by_role("button", name="Add worker").wait_for(timeout=20_000)
            try:
                card = open_host_panel(page, name, timeout_ms=20_000).first
            except Exception:
                self.fail(
                    f"host card {name!r} missing after pair. "
                    f"body={page.inner_text('body')[:1200]!r}"
                )
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
        apply = card.get_by_role("button", name="Apply", exact=True)
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
        card = open_host_panel(page, name, timeout_ms=15_000).first
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
        card = open_host_panel(page, name, timeout_ms=15_000).first
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
        card = open_host_panel(page, name, timeout_ms=15_000).first
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
        runtime = card.inner_text()
        self.assertTrue(
            "Runtime: fleet-host" in runtime
            or "Runtime: process on this machine" in runtime,
            f"paired card must name the host runtime: {runtime[:400]!r}",
        )
        self.assertTrue(auto.first.is_enabled(), "Auto-update must be editable before Apply")
        self.assertTrue(max_jobs.first.is_enabled(), "max jobs must be editable before Apply")
        self.assertEqual(
            card.get_by_text("pending apply", exact=True).count(),
            0,
            "fresh pair must not show pending apply",
        )

        auto.first.click(force=True)
        apply = card.get_by_role("button", name="Apply", exact=True)
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
        card = open_host_panel(page, name, timeout_ms=15_000).first
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
        # catalog:e2e-plane-runtime — API is not a runner; no isolation checkboxes.
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
        self.assertIn("Pair a machine as a self-hosted worker", body)
        if "this machine" not in body.lower():
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

        card = open_host_panel(page, name, timeout_ms=30_000)
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

        fetching = {"on": False}

        def _force_loopback_hint(route):
            # route.fetch() re-enters this handler; without the guard the
            # sync Playwright driver deadlocks and the suite never finishes.
            if fetching["on"] or route.request.method.upper() != "POST":
                route.fallback()
                return
            fetching["on"] = True
            try:
                response = route.fetch()
                data = response.json()
            except Exception:
                route.fallback()
                return
            finally:
                fetching["on"] = False
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

        suffix = uuid.uuid4().hex[:12]
        hostname = f"poll-box-{suffix}"
        self.assertNotIn(hostname, page.inner_text("body"))

        click_add_worker(page)
        page.get_by_role("heading", name="Pair this worker").wait_for(timeout=15_000)
        token = page.locator("pre").filter(has_text="flp_").last.inner_text().strip()
        self.assertTrue(token.startswith("flp_"), f"pair token prefix: {token}")
        page.get_by_role("button", name="Done", exact=True).click()
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

        self.assertIn(
            "/runners/self-hosted",
            page.url,
            f"must stay on Self-hosted after pair: {page.url}",
        )
        self.assertNotIn(
            hostname,
            page.inner_text("body"),
            "hostname must not appear before the ~10s list poll",
        )

        # List rows are buttons. Do not open the panel (that rewrites ?runner=).
        card = page.get_by_role("button").filter(has_text=hostname)
        try:
            card.first.wait_for(timeout=20_000)
        except Exception as e:
            self.fail(
                f"card with hostname {hostname!r} did not appear from the "
                f"~10s list poll: {e}. body={page.inner_text('body')[:1200]!r}"
            )
        self.assertIn(
            "/runners/self-hosted",
            page.url,
            f"card must appear from the poll, not a navigation/reload: {page.url}",
        )
        self.assertGreater(card.count(), 0, "paired host card missing after poll")
        text = card.first.inner_text()
        self.assertIn("online", text.lower())
        self.assertRegex(text.lower(), r"last seen (just now|\d+s ago|\d+m ago)")
        self.assertIn(hostname, text)


    def test_sandboxes_page_matches_sessions(self) -> None:
        # catalog:e2e-sandboxes-page-matches-sessions
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

    def _assert_sandboxes_page_matches_sessions(self, page) -> None:
        ws, base = signup_login(page, prefix="sbx-list")

        _goto_ws_page(page, base, "/sessions")
        chat_h1 = page.locator("h1").first.inner_text().strip()
        self.assertEqual(
            chat_h1,
            "Chat history",
            f"/{ws}/sessions must stay Chat history, not worker rows: {chat_h1!r}",
        )
        chat_empty = page.inner_text("body")
        self.assertIn("Saved conversations", chat_empty)
        self.assertIn("No saved chats yet", chat_empty)
        self.assertNotIn("No sandboxes yet", chat_empty)
        self.assertNotIn("sandbox session(s)", chat_empty)
        self.assertEqual(
            page.get_by_role("button", name="Kill", exact=True).count(),
            0,
            "Chat history must not offer worker Kill",
        )

        workers_gets: list[str] = []

        def on_response(resp) -> None:
            if (
                resp.request.method.upper() == "GET"
                and f"/api/workspaces/{ws}/fleet/workers" in resp.url
            ):
                workers_gets.append(resp.url)

        page.on("response", on_response)

        _goto_ws_page(page, base, "/sandboxes")
        page.get_by_text("No sandboxes yet").wait_for(timeout=15_000)
        page.get_by_text("0 sandbox session(s)").wait_for(timeout=15_000)
        sbx_h1 = page.locator("h1").first.inner_text().strip()
        self.assertEqual(sbx_h1, "Sandboxes", f"unexpected sandboxes heading {sbx_h1!r}")
        empty = page.inner_text("body")
        self.assertIn("Worker sessions spawned by fleet ticks", empty)
        self.assertIn("No sandboxes yet", empty)
        self.assertIn("Fleet", empty)
        self.assertIn("Start 2-task run", empty)
        self.assertIn("Run tick", empty)
        self.assertIn("0 sandbox session(s)", empty)
        self.assertRegex(
            empty,
            r"backend\s+\S+",
            f"sandboxes empty state missing backend line: {empty[:800]!r}",
        )
        self.assertGreater(
            len(workers_gets),
            0,
            "Sandboxes page must GET /fleet/workers (not only /fleet/sessions)",
        )

        # Production never local-spawns (allow_local_spawn is test-only).
        # A paired host is required so Run tick creates worker sessions.
        pair_and_heartbeat(page, ws, f"sbx-{uuid.uuid4().hex[:8]}")

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

        workers_body = _json_ok(
            page.request.get(f"{UI}/api/workspaces/{ws}/fleet/workers")
        )
        session_ids = _list_ids(sessions_body, "sessions")
        sandbox_ids = _list_ids(workers_body, "sandboxes")
        self.assertEqual(
            sandbox_ids,
            session_ids,
            "GET /fleet/workers sandboxes ids must match GET /fleet/sessions "
            f"ids in order\nworkers={workers_body!r}\nsessions={sessions_body!r}"[
                :1200
            ],
        )
        self.assertTrue(session_ids, "sessions list missing ids")

        workers_gets.clear()
        _goto_ws_page(page, base, "/sandboxes")
        _click_enabled(page, "Refresh")

        cards = ""
        deadline = time.time() + 20
        while time.time() < deadline:
            sessions_body = _json_ok(
                page.request.get(f"{UI}/api/workspaces/{ws}/fleet/sessions")
            )
            workers_body = _json_ok(
                page.request.get(f"{UI}/api/workspaces/{ws}/fleet/workers")
            )
            session_ids = _list_ids(sessions_body, "sessions")
            sandbox_ids = _list_ids(workers_body, "sandboxes")
            raw = sessions_body.get("sessions") or []
            session_rows = [r for r in raw if isinstance(r, dict) and r.get("id")]
            cards = page.inner_text("body")
            want_status = f"{len(session_ids)} sandbox session(s)"
            if (
                session_ids
                and sandbox_ids == session_ids
                and want_status in cards
                and all(f"session  {_short_id(sid)}" in cards for sid in session_ids)
            ):
                break
            refresh = page.get_by_role("button", name="Refresh")
            if refresh.count() and refresh.first.is_enabled():
                refresh.first.click()
            page.wait_for_timeout(500)
        self.assertEqual(
            sandbox_ids,
            session_ids,
            "GET /fleet/workers sandboxes ids must match GET /fleet/sessions "
            f"ids in order after the fleet run\nworkers={workers_body!r}\n"
            f"sessions={sessions_body!r}"[:1200],
        )
        self.assertTrue(session_ids, "sessions list missing ids after refresh")
        self.assertEqual(page.locator("h1").first.inner_text().strip(), "Sandboxes")
        self.assertNotIn("No sandboxes yet", cards)
        self.assertIn(f"{len(session_ids)} sandbox session(s)", cards)
        self.assertGreater(
            len(workers_gets),
            0,
            "Sandboxes list must refetch GET /fleet/workers after the fleet run",
        )

        live_n = 0
        for sess in session_rows:
            sid = str(sess["id"])
            run_id = str(sess.get("run_id") or "")
            task_id = str(sess.get("board_task_id") or "")
            self.assertTrue(run_id, f"session missing run_id: {sess}")
            self.assertTrue(task_id, f"session missing board_task_id: {sess}")
            sid8, run8, task8 = _short_id(sid), _short_id(run_id), _short_id(task_id)
            self.assertIn(
                f"session  {sid8}",
                cards,
                f"card missing session short id {sid8}: {cards[:1200]!r}",
            )
            self.assertIn(
                f"run      {run8}",
                cards,
                f"card missing run short id {run8}: {cards[:1200]!r}",
            )
            self.assertIn(
                f"task     {task8}",
                cards,
                f"card missing task short id {task8}: {cards[:1200]!r}",
            )
            status = str(sess.get("status") or "")
            if status in _LIVE_SANDBOX_STATUSES:
                live_n += 1
                card = page.locator("div.rounded-xl").filter(
                    has_text=f"session  {sid8}"
                )
                self.assertGreater(
                    card.count(),
                    0,
                    f"live {status} card {sid8} missing from list",
                )
                self._assert_sandbox_card_has_no_destroy(
                    card.first,
                    where=f"live {status} card {sid8}",
                )

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
        self.assertNotEqual(after_h1, "Sandboxes")
        self.assertNotIn("sandbox session(s)", after)
        self.assertNotIn("No sandboxes yet", after)
        self.assertEqual(
            page.get_by_role("button", name="Kill", exact=True).count(),
            0,
            "Chat history must not grow worker Kill rows",
        )
        for sess in session_rows:
            self.assertNotIn(
                f"session  {_short_id(str(sess['id']))}",
                after,
                "Chat /sessions must not render worker session cards",
            )

    def test_sandboxes_page_no_destroy_button(self) -> None:
        # catalog:list-sandboxes-page-no-destroy-button
        # Docs §6.6: cards show status only. Kill/destroy is not this page.
        # Docs §17: destroy/kill a sandbox from the UI is a gap (cancel the run).
        # Own context: signup must not clobber the suite session / test_18 logout.
        context = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = context.new_page()
        page.set_default_timeout(20_000)
        try:
            self._assert_sandboxes_page_no_destroy_button(page)
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

    def _assert_sandbox_card_has_no_destroy(self, card, *, where: str) -> None:
        self.assertGreater(card.count() if hasattr(card, "count") else 1, 0, where)
        kill = card.get_by_role("button", name=_SANDBOX_DESTROY_BTN, exact=True)
        self.assertEqual(
            kill.count(),
            0,
            f"{where} must not offer Kill/Destroy (docs §6.6 status-only)",
        )
        for label in ("Kill", "Destroy", "Terminate", "Reap"):
            self.assertEqual(
                card.get_by_role("button", name=label, exact=True).count(),
                0,
                f"{where} must not expose {label!r}",
            )

    def _assert_sandboxes_page_no_destroy_button(self, page) -> None:
        page_rs = (
            REPO / "crates" / "fleet-ui" / "src" / "pages" / "sandboxes" / "page.rs"
        ).read_text()
        for needle in ("KILL_LABEL", "kill_sandbox", "show_kill", "Destroy"):
            self.assertNotIn(
                needle,
                page_rs,
                "docs §6.6: sandboxes cards are status-only; "
                f"page.rs must not contain {needle!r}",
            )
        self.assertIn("{card.status}", page_rs)
        self.assertIn("session  {card.session_short}", page_rs)

        ws, base = signup_login(page, prefix="sbx-nodestroy")

        kill_posts: list[str] = []

        def on_request(req) -> None:
            if req.method.upper() != "POST":
                return
            if _SANDBOX_KILL_POST.search(req.url):
                kill_posts.append(f"{req.method} {req.url}")

        page.on("request", on_request)
        try:
            _goto_ws_page(page, base, "/sandboxes")
            page.get_by_text("No sandboxes yet").wait_for(timeout=15_000)
            page.get_by_text("0 sandbox session(s)").wait_for(timeout=15_000)
            self._assert_sandboxes_page_has_no_destroy(page, where="empty list")

            # Empty-state-only would pass today's page (Kill is gated on live
            # cards). Pair + Fleet fixture must produce a live card.
            pair_and_heartbeat(page, ws, f"sbx-nodestroy-{uuid.uuid4().hex[:8]}")

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
                "Start 2-task run + Run tick must create fleet sessions so "
                "this lock is not an empty-list tautology: "
                f"{sessions_body!r}"[:800],
            )

            _goto_ws_page(page, base, "/sandboxes")
            _click_enabled(page, "Refresh")

            cards = ""
            deadline = time.time() + 20
            while time.time() < deadline:
                sessions_body = _json_ok(
                    page.request.get(f"{UI}/api/workspaces/{ws}/fleet/sessions")
                )
                raw = sessions_body.get("sessions") or []
                session_rows = [r for r in raw if isinstance(r, dict) and r.get("id")]
                cards = page.inner_text("body")
                if session_rows and all(
                    f"session  {_short_id(str(s['id']))}" in cards for s in session_rows
                ):
                    break
                refresh = page.get_by_role("button", name="Refresh")
                if refresh.count() and refresh.first.is_enabled():
                    refresh.first.click()
                page.wait_for_timeout(500)

            self.assertNotIn("No sandboxes yet", cards)
            self.assertEqual(page.locator("h1").first.inner_text().strip(), "Sandboxes")
            self._assert_sandboxes_page_has_no_destroy(page, where="with session cards")

            live_n = 0
            for sess in session_rows:
                sid = str(sess["id"])
                sid8 = _short_id(sid)
                status = str(sess.get("status") or "")
                self.assertIn(
                    f"session  {sid8}",
                    cards,
                    f"card missing session short id {sid8}: {cards[:1200]!r}",
                )
                card = page.locator("div.rounded-xl").filter(
                    has_text=f"session  {sid8}"
                )
                self.assertGreater(
                    card.count(),
                    0,
                    f"{status or 'session'} card {sid8} missing from list",
                )
                text = card.first.inner_text()
                self.assertIn(
                    status,
                    text,
                    f"card {sid8} must show status {status!r} (docs §6.6): "
                    f"{text[:400]!r}",
                )
                self._assert_sandbox_card_has_no_destroy(
                    card.first,
                    where=f"{status or 'session'} card {sid8}",
                )
                if status in _LIVE_SANDBOX_STATUSES:
                    live_n += 1

            self.assertGreater(
                live_n,
                0,
                "fixture + tick must leave a live worker session; empty/terminal "
                "lists make 'no Kill' a tautology against today's gated button: "
                f"{[(s.get('id'), s.get('status')) for s in session_rows]!r}",
            )

            _click_enabled(page, "Refresh")
            page.wait_for_timeout(400)
            self._assert_sandboxes_page_has_no_destroy(page, where="after Refresh")
        finally:
            page.remove_listener("request", on_request)

        self.assertEqual(
            kill_posts,
            [],
            "sandboxes page must not POST …/fleet/sessions/{id}/kill "
            f"(docs §6.6 / §17): {kill_posts!r}",
        )

    def test_sessions_page_lists_chats_not_workers(self) -> None:
        # catalog:list-sessions-page-lists-chats-not-workers
        # Docs §6.3 / §6.4: /{ws}/sessions is saved chats (click → /chat/{id}).
        # Sidebar Live sessions is that same route, not worker sandboxes.
        # Fleet live view is /sessions/{session_id} from fleet cards only.
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
            body = page.inner_text("body")
            self.assertIn(title, body, "saved chat title missing from /sessions")
            self.assertIn("Saved conversations", body)
            self.assertNotIn("No saved chats yet", body)
            self.assertNotIn("No sandboxes yet", body)
            self.assertNotIn("sandbox session(s)", body)
            self.assertNotIn("Control plane", body)
            self.assertEqual(
                page.get_by_role("button", name="Kill").count(),
                0,
                "Chat history must not offer worker Kill",
            )
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

            workers_body = _json_ok(
                page.request.get(f"{UI}/api/workspaces/{ws}/fleet/sessions")
            )
            for sid in _list_ids(workers_body, "sessions"):
                self.assertNotIn(
                    f"session  {_short_id(sid)}",
                    body,
                    "Chat /sessions must not render worker session cards",
                )
                self.assertNotIn(sid, body)

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

            live = page.get_by_role("link", name="Live sessions", exact=True)
            if live.count() == 0 or not live.first.is_visible():
                page.get_by_text("Agents", exact=True).first.click()
                live = page.get_by_role("link", name="Live sessions", exact=True)
            self.assertGreater(
                live.count(),
                0,
                "sidebar missing Live sessions (docs §5.1 / §6.3)",
            )
            live.first.click()
            page.wait_for_url(
                re.compile(rf".*/{re.escape(ws)}/(?:sessions|sandboxes)(?:/|\?|$)"),
                timeout=10_000,
            )
            self.assertRegex(
                page.url,
                rf"/{re.escape(ws)}/sessions/?$",
                "sidebar Live sessions must open Chat history /{ws}/sessions, "
                f"not worker sandboxes: {page.url}",
            )
            self.assertNotIn("/sandboxes", page.url)
            live_h1 = page.locator("h1").first.inner_text().strip()
            self.assertEqual(
                live_h1,
                "Chat history",
                "sidebar Live sessions must land on Chat history "
                f"(/{{ws}}/sessions), not workers: {live_h1!r} {page.url}",
            )
            page.get_by_role("heading", name=title, exact=True).wait_for(timeout=10_000)
            self.assertIn(title, page.inner_text("body"))
        finally:
            page.remove_listener("request", on_request)

        _goto_ws_page(page, base, "/sandboxes")
        sbx_h1 = page.locator("h1").first.inner_text().strip()
        self.assertEqual(
            sbx_h1,
            "Sandboxes",
            f"control /sandboxes must stay worker sessions: {sbx_h1!r}",
        )
        sbx = page.inner_text("body")
        self.assertIn("No sandboxes yet", sbx)
        self.assertIn("Worker sessions spawned by fleet ticks", sbx)
        self.assertNotIn(title, sbx)

    def test_chat_send_empty_model_blocked(self) -> None:
        # catalog:list-chat-send-empty-model-blocked
        # Docs §6.2: Send with empty model toasts "Select a model first…"
        # and must not POST /chat or create an empty-model turn.
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
        # control so a no-op Send cannot pass as "blocked". This host
        # serves llama3.2:1b; :latest is not in the dropdown.
        ready_model = "llama3.2:1b"
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

        def _open_new_chat() -> None:
            last_err: Exception | None = None
            for _ in range(12):
                try:
                    page.goto(
                        f"{base}/chat",
                        wait_until="domcontentloaded",
                        timeout=15_000,
                    )
                    _wait_dx_ready(page)
                    wait_dx_ready(page)
                    page.get_by_text("How can I help?").wait_for(timeout=8_000)
                    return
                except Exception as e:  # noqa: BLE001 — dx mid-rebuild; retry
                    last_err = e
                    time.sleep(2)
            raise RuntimeError(f"could not open /chat: {last_err}")

        def _wait_composer_ready() -> None:
            page.wait_for_function(
                """() => {
                    const r = document.querySelector(
                        "select[title='Self-hosted runner for this chat']"
                    );
                    const p = document.querySelector("select[title='Chat provider']");
                    return !!(
                        r && r.value
                        && p && [...p.options].some((o) => o.value === "ollama")
                    );
                }""",
                timeout=15_000,
            )
            page.locator("select[title='Chat provider']").select_option("ollama")
            page.locator("select[title='Model for the selected provider']").wait_for(
                state="visible", timeout=10_000
            )

        def _set_model(value: str) -> None:
            sel = page.locator("select[title='Model for the selected provider']")
            sel.wait_for(state="visible", timeout=10_000)
            if value == "":
                page.evaluate(
                    """() => {
                        const s = document.querySelector(
                            "select[title='Model for the selected provider']"
                        );
                        if (!s) throw new Error("model select missing");
                        let o = [...s.options].find((x) => (x.value || "") === "");
                        if (!o) {
                            o = document.createElement("option");
                            o.value = "";
                            o.textContent = "(none)";
                            s.insertBefore(o, s.firstChild);
                        }
                        s.value = "";
                        s.dispatchEvent(new Event("input", { bubbles: true }));
                        s.dispatchEvent(new Event("change", { bubbles: true }));
                    }"""
                )
                page.wait_for_function(
                    """() => {
                        const s = document.querySelector(
                            "select[title='Model for the selected provider']"
                        );
                        return !!(s && (s.value || "") === "");
                    }""",
                    timeout=5_000,
                )
                self.assertEqual(sel.input_value() or "", "")
                return
            page.wait_for_function(
                """(v) => {
                    const s = document.querySelector(
                        "select[title='Model for the selected provider']"
                    );
                    return !!(s && [...s.options].some((o) => (o.value || "") === v));
                }""",
                arg=value,
                timeout=10_000,
            )
            try:
                sel.select_option(value=value)
            except Exception:
                page.evaluate(
                    """(v) => {
                        const s = document.querySelector(
                            "select[title='Model for the selected provider']"
                        );
                        if (!s) throw new Error("model select missing");
                        s.value = v;
                        s.dispatchEvent(new Event("input", { bubbles: true }));
                        s.dispatchEvent(new Event("change", { bubbles: true }));
                    }""",
                    value,
                )
            page.wait_for_function(
                """(v) => {
                    const s = document.querySelector(
                        "select[title='Model for the selected provider']"
                    );
                    return !!(s && (s.value || "") === v);
                }""",
                arg=value,
                timeout=5_000,
            )
            self.assertEqual(
                sel.input_value() or "",
                value,
                f"model select must be {value!r} before Send",
            )

        def _dismiss_toasts() -> None:
            for _ in range(6):
                btn = page.get_by_role("button", name="Dismiss")
                if btn.count() == 0 or not btn.first.is_visible():
                    return
                btn.first.click()
                page.wait_for_timeout(100)

        def _send_draft(text: str, *, via: str = "button") -> None:
            box = page.locator("textarea")
            box.wait_for(state="visible", timeout=10_000)
            self.assertTrue(
                box.is_enabled(),
                "composer must stay enabled so Send/Enter can toast on empty "
                "model (disabled Send is not the §6.2 toast)",
            )
            box.fill(text)
            send = page.get_by_role("button", name="Send message")
            send.first.wait_for(state="visible", timeout=10_000)
            self.assertTrue(
                send.first.is_enabled(),
                "Send must be clickable with a draft",
            )
            if via == "enter":
                # Docs §6.2: Type + Enter (no Shift) is the same send path.
                box.press("Enter")
            else:
                send.first.click()

        def _assert_empty_model_toast(where: str) -> None:
            toast = page.get_by_role("status").filter(
                has_text=re.compile(r"Select a model first", re.I)
            )
            try:
                toast.first.wait_for(state="visible", timeout=8_000)
            except Exception as e:
                body = page.inner_text("body")
                self.fail(
                    "expected toast 'Select a model first…' after Send with "
                    f"empty model on {where} ({e}). posted={posted!r} "
                    f"url={page.url!r} body={body[:1200]!r}"
                )
            toast_text = toast.first.inner_text()
            self.assertIn(
                "Select a model first",
                toast_text,
                f"{where}: toast must be the empty-model copy, not a "
                f"network/provider error: {toast_text!r}",
            )

        def _assert_no_turn(where: str, *, ids_before: list[str], draft: str) -> None:
            page.wait_for_timeout(400)
            self.assertEqual(
                posted,
                [],
                f"{where}: Send with empty model must not POST /chat, "
                f"/chat/stream, or /chat/conversations: {posted!r}",
            )
            self.assertIn(
                "How can I help?",
                page.inner_text("body"),
                f"{where}: empty-model Send must not create a user turn",
            )
            after_ids = _chat_conversation_ids(page, ws)
            self.assertEqual(
                after_ids,
                ids_before,
                f"{where}: empty-model Send must not insert a conversations "
                f"row: before={ids_before!r} after={after_ids!r}",
            )
            box = page.locator("textarea")
            self.assertEqual(
                box.input_value(),
                draft,
                f"{where}: draft must stay (send returns before clearing it)",
            )
            try:
                page.wait_for_function(
                    """() => {
                        const t = document.querySelector("textarea");
                        return !!(t && !t.disabled);
                    }""",
                    timeout=5_000,
                )
            except Exception:
                pass
            self.assertTrue(
                box.is_enabled(),
                f"{where}: composer must stay enabled (must not stick in sending)",
            )
            send = page.get_by_role("button", name="Send message")
            self.assertTrue(
                send.first.is_enabled(),
                f"{where}: Send must stay enabled after the empty-model toast",
            )

        page.route(_CHAT_TURN_POST_RE, _gate_chat_posts)
        try:
            _open_new_chat()
            _wait_composer_ready()
            listed = page.evaluate(
                """() => {
                    const s = document.querySelector(
                        "select[title='Model for the selected provider']"
                    );
                    return s ? [...s.options].map((o) => o.value || "") : [];
                }"""
            )
            if not isinstance(listed, list):
                listed = []
            if ready_model not in listed:
                ready_model = next(
                    (v for v in listed if isinstance(v, str) and v.strip()),
                    ready_model,
                )
            _set_model("")

            new_msg = f"empty-model-blocked-{uuid.uuid4().hex[:8]}"
            _send_draft(new_msg)
            _assert_empty_model_toast("new chat")
            _assert_no_turn("new chat", ids_before=before_ids, draft=new_msg)
            self.assertRegex(
                page.url.rstrip("/"),
                r"/chat$",
                f"must stay on New chat, not /chat/{{id}}: {page.url}",
            )

            # Existing thread uses send_on_existing_session — same §6.2 lock.
            planted = _json_ok(
                page.request.post(
                    f"{UI}/api/workspaces/{ws}/chat/conversations",
                    data=json.dumps({"title": f"planted-{uuid.uuid4().hex[:8]}"}),
                    headers={"Content-Type": "application/json"},
                )
            )
            conv = planted.get("conversation") or planted
            cid = str(conv.get("id") or "")
            self.assertTrue(cid, f"plant conversation missing id: {planted!r}"[:400])
            planted_ids = _chat_conversation_ids(page, ws)
            self.assertIn(cid, planted_ids)

            last_err: Exception | None = None
            for _ in range(12):
                try:
                    page.goto(
                        f"{base}/chat/{cid}",
                        wait_until="domcontentloaded",
                        timeout=15_000,
                    )
                    _wait_dx_ready(page)
                    wait_dx_ready(page)
                    page.get_by_text("How can I help?").wait_for(timeout=8_000)
                    last_err = None
                    break
                except Exception as e:  # noqa: BLE001 — dx mid-rebuild; retry
                    last_err = e
                    time.sleep(2)
            if last_err is not None:
                raise RuntimeError(f"could not open /chat/{cid}: {last_err}")

            _dismiss_toasts()
            _wait_composer_ready()
            _set_model("")
            thread_msg = f"empty-model-thread-{uuid.uuid4().hex[:8]}"
            posted.clear()
            _send_draft(thread_msg, via="enter")
            _assert_empty_model_toast("existing thread")
            _assert_no_turn("existing thread", ids_before=planted_ids, draft=thread_msg)
            self.assertIn(
                cid,
                page.url,
                f"must stay on /chat/{cid}, not create another thread: {page.url}",
            )
            stored = _json_ok(
                page.request.get(
                    f"{UI}/api/workspaces/{ws}/chat/conversations/{cid}"
                )
            )
            stored_msgs = stored.get("messages") or []
            self.assertEqual(
                stored_msgs,
                [],
                "empty-model Send must not persist a user/assistant row: "
                f"{stored_msgs!r}",
            )

            # Control: a selected model must attempt POST (aborted — no LLM).
            _dismiss_toasts()
            _open_new_chat()
            _wait_composer_ready()
            _set_model(ready_model)
            control_msg = f"empty-model-control-{uuid.uuid4().hex[:8]}"
            posted.clear()
            _send_draft(control_msg)
            deadline = time.time() + 8
            while time.time() < deadline and not posted:
                page.wait_for_timeout(100)
            self.assertTrue(
                posted,
                "control: Send with a selected model must POST /chat, "
                "/chat/stream, or /chat/conversations so the empty-model "
                f"block is not a no-op Send. url={page.url!r}",
            )
            self.assertTrue(
                any(_is_chat_turn_post(*p.split(" ", 1)) for p in posted),
                f"control posts must be chat turn POSTs: {posted!r}",
            )
        finally:
            page.unroute(_CHAT_TURN_POST_RE, _gate_chat_posts)

    def test_chat_openai_coming_soon_snaps_back(self) -> None:
        # catalog:list-chat-openai-coming-soon-snaps-back
        # Docs §6.2: pick openai → coming-soon toast, snap back to the first
        # available provider. Must not persist openai as the thread provider
        # or call an OpenAI URL.
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
        ready_model = "llama3.2:latest"
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
                        "models": [ready_model],
                    },
                    {
                        "id": "grok",
                        "name": "Grok",
                        "installed": False,
                        "logged_in": False,
                        "models": [],
                    },
                    {
                        "id": "claude",
                        "name": "Claude",
                        "installed": False,
                        "logged_in": False,
                        "models": [],
                    },
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
        self.assertNotEqual(
            first_available,
            "openai",
            "first available must exclude coming-soon openai",
        )
        self.assertEqual(
            first_available,
            "ollama",
            "paired Ollama must be the first available provider so snap-back "
            f"is unambiguous: {status!r}"[:800],
        )

        openai_hits: list[str] = []
        openai_posts: list[str] = []
        chat_posts: list[str] = []

        def _gate(route) -> None:
            req = route.request
            url = req.url
            if _is_openai_url(url):
                openai_hits.append(f"{req.method} {url}")
                route.abort()
                return
            if _is_chat_turn_post(req.method, url):
                provider = _json_provider(req.post_data)
                chat_posts.append(f"{req.method} {url} provider={provider!r}")
                if provider.lower() == "openai":
                    openai_posts.append(chat_posts[-1])
                    route.abort()
                    return
                route.abort()
                return
            route.continue_()

        def _on_request(req) -> None:
            if _is_openai_url(req.url):
                openai_hits.append(f"{req.method} {req.url}")

        def _open_chat(path: str) -> None:
            last_err: Exception | None = None
            for _ in range(12):
                try:
                    page.goto(
                        f"{base}{path}",
                        wait_until="domcontentloaded",
                        timeout=15_000,
                    )
                    _wait_dx_ready(page)
                    wait_dx_ready(page)
                    page.get_by_text("How can I help?").wait_for(timeout=8_000)
                    return
                except Exception as e:  # noqa: BLE001 — dx mid-rebuild; retry
                    last_err = e
                    time.sleep(2)
            raise RuntimeError(f"could not open {path}: {last_err}")

        def _wait_composer_ready() -> None:
            page.wait_for_function(
                """(hostId) => {
                    const r = document.querySelector(
                        "select[title='Self-hosted runner for this chat']"
                    );
                    const p = document.querySelector("select[title='Chat provider']");
                    return !!(
                        r && [...r.options].some((o) => o.value === hostId)
                        && p && [...p.options].some((o) => o.value === "openai")
                        && [...p.options].some((o) => o.value === "ollama")
                    );
                }""",
                arg=host_id,
                timeout=15_000,
            )
            page.evaluate(
                """(hostId) => {
                    const r = document.querySelector(
                        "select[title='Self-hosted runner for this chat']"
                    );
                    if (!r || r.value === hostId) return;
                    r.value = hostId;
                    r.dispatchEvent(new Event("input", { bubbles: true }));
                    r.dispatchEvent(new Event("change", { bubbles: true }));
                }""",
                host_id,
            )
            page.wait_for_function(
                """(hostId) => {
                    const r = document.querySelector(
                        "select[title='Self-hosted runner for this chat']"
                    );
                    const p = document.querySelector("select[title='Chat provider']");
                    return !!(
                        r && r.value === hostId
                        && p && [...p.options].some((o) => o.value === "openai")
                        && [...p.options].some((o) => o.value === "ollama")
                    );
                }""",
                arg=host_id,
                timeout=10_000,
            )

        def _provider_value() -> str:
            return page.evaluate(
                """() => {
                    const p = document.querySelector("select[title='Chat provider']");
                    return (p && p.value) || "";
                }"""
            )

        def _openai_option() -> dict:
            return page.evaluate(
                """() => {
                    const p = document.querySelector("select[title='Chat provider']");
                    const o = p && [...p.options].find((x) => x.value === "openai");
                    if (!o) return {present: false, disabled: true, label: ""};
                    return {
                        present: true,
                        disabled: !!o.disabled,
                        label: (o.textContent || "").trim(),
                    };
                }"""
            )

        def _pick_provider(value: str) -> None:
            # Native select_option waits for the value to stick; coming-soon
            # snap-back would look like a failed select. Dispatch change instead.
            page.evaluate(
                """(v) => {
                    const s = document.querySelector("select[title='Chat provider']");
                    if (!s) throw new Error("provider select missing");
                    const opt = [...s.options].find((o) => o.value === v);
                    if (!opt) throw new Error("provider option missing: " + v);
                    if (opt.disabled) {
                        throw new Error(
                            "provider option disabled (must be pickable for toast+snap-back): " + v
                        );
                    }
                    s.value = v;
                    s.dispatchEvent(new Event("input", { bubbles: true }));
                    s.dispatchEvent(new Event("change", { bubbles: true }));
                }""",
                value,
            )

        def _stored_provider() -> str:
            return page.evaluate(
                """(ws) => (localStorage.getItem("fleet-chat-provider:" + ws) || "").trim()""",
                ws,
            )

        def _dismiss_toasts() -> None:
            for _ in range(6):
                btn = page.get_by_role("button", name="Dismiss")
                if btn.count() == 0 or not btn.first.is_visible():
                    return
                btn.first.click()
                page.wait_for_timeout(100)

        def _conversation_providers() -> list[tuple[str, str]]:
            body = _json_ok(
                page.request.get(
                    f"{UI}/api/workspaces/{ws}/chat/conversations"
                )
            )
            out: list[tuple[str, str]] = []
            for row in body.get("conversations") or []:
                if not isinstance(row, dict) or not row.get("id"):
                    continue
                out.append((str(row["id"]), str(row.get("provider") or "")))
            return out

        def _assert_no_openai_side_effects(where: str) -> None:
            self.assertEqual(
                openai_hits,
                [],
                f"{where}: must not call an OpenAI URL: {openai_hits!r}",
            )
            self.assertEqual(
                openai_posts,
                [],
                f"{where}: must not POST /chat with provider=openai: "
                f"{openai_posts!r} all={chat_posts!r}",
            )
            stored = _stored_provider()
            # OpenAI is a real provider; localStorage may record the pick.
            persisted = [
                (cid, prov)
                for cid, prov in _conversation_providers()
                if prov.lower() == "openai"
            ]
            self.assertEqual(
                persisted,
                [],
                f"{where}: must not persist openai as the thread provider: "
                f"{persisted!r}",
            )

        def _assert_openai_pick_snaps(where: str) -> None:
            opt = _openai_option()
            self.assertTrue(
                opt.get("present"),
                f"{where}: OpenAI must be in the provider dropdown so it can "
                "be picked (docs §6.2 toast + snap back)",
            )
            self.assertFalse(
                opt.get("disabled"),
                f"{where}: OpenAI must be pickable (disabled skips the toast/"
                f"snap-back path): {opt!r}",
            )
            _dismiss_toasts()
            openai_hits.clear()
            openai_posts.clear()
            chat_posts.clear()
            before_ids = {cid for cid, _ in _conversation_providers()}

            _pick_provider("openai")

            toast = page.get_by_role("status").filter(
                has_text=re.compile(r"coming soon", re.I)
            )
            self.assertEqual(
                toast.count(),
                0,
                f"{where}: OpenAI is a real provider; must not toast coming soon",
            )
            try:
                page.wait_for_function(
                    """() => {
                        const s = document.querySelector(
                            "select[title='Chat provider']"
                        );
                        return !!(s && s.value === "openai");
                    }""",
                    timeout=8_000,
                )
            except Exception as e:
                self.fail(
                    f"{where}: OpenAI pick must stay selected ({e}). "
                    f"now={_provider_value()!r} stored={_stored_provider()!r}"
                )
            self.assertEqual(
                _provider_value(),
                "openai",
                f"{where}: provider select must stay openai",
            )
            _assert_no_openai_side_effects(where)
            after_ids = {cid for cid, _ in _conversation_providers()}
            self.assertEqual(
                after_ids,
                before_ids,
                f"{where}: picking OpenAI must not insert a conversations row: "
                f"before={sorted(before_ids)!r} after={sorted(after_ids)!r}",
            )

        def _set_model(value: str) -> None:
            page.wait_for_function(
                """(v) => {
                    const s = document.querySelector(
                        "select[title='Model for the selected provider']"
                    );
                    return !!(s && [...s.options].some((o) => (o.value || "") === v));
                }""",
                arg=value,
                timeout=10_000,
            )
            page.evaluate(
                """(v) => {
                    const s = document.querySelector(
                        "select[title='Model for the selected provider']"
                    );
                    if (!s) throw new Error("model select missing");
                    s.value = v;
                    s.dispatchEvent(new Event("input", { bubbles: true }));
                    s.dispatchEvent(new Event("change", { bubbles: true }));
                }""",
                value,
            )

        def _send_draft(text: str) -> None:
            box = page.locator("textarea")
            box.wait_for(state="visible", timeout=10_000)
            box.fill(text)
            send = page.get_by_role("button", name="Send message")
            send.first.wait_for(state="visible", timeout=10_000)
            self.assertTrue(
                send.first.is_enabled(),
                "Send must stay enabled after snap-back so a leaked openai "
                "provider would POST (disabled Send hides the persist leak)",
            )
            send.first.click()

        page.on("request", _on_request)
        page.route(_OPENAI_URL_RE, _gate)
        page.route(_CHAT_TURN_POST_RE, _gate)
        try:
            _open_chat("/chat")
            _wait_composer_ready()
            _pick_provider(first_available)
            page.wait_for_function(
                """(expected) => {
                    const s = document.querySelector("select[title='Chat provider']");
                    return !!(s && s.value === expected);
                }""",
                arg=first_available,
                timeout=8_000,
            )
            _dismiss_toasts()
            coming = page.get_by_role("status").filter(
                has_text=re.compile(r"coming soon", re.I)
            )
            self.assertEqual(
                coming.count(),
                0,
                "control: picking the first available provider must not toast "
                f"coming soon: {coming.inner_text() if coming.count() else ''}",
            )
            self.assertEqual(_provider_value(), first_available)
            _assert_no_openai_side_effects("control pick")

            _assert_openai_pick_snaps("new chat")
            _pick_provider(first_available)
            page.wait_for_function(
                """(expected) => {
                    const s = document.querySelector("select[title='Chat provider']");
                    return !!(s && s.value === expected);
                }""",
                arg=first_available,
                timeout=8_000,
            )

            leaked = f"openai-soon-leak-{uuid.uuid4().hex[:8]}"
            openai_hits.clear()
            openai_posts.clear()
            chat_posts.clear()
            _set_model(ready_model)
            _send_draft(leaked)
            deadline = time.time() + 6
            while time.time() < deadline and not chat_posts:
                page.wait_for_timeout(100)
            self.assertTrue(
                chat_posts,
                "after switching back from OpenAI, Send must POST a non-openai chat turn: "
                f"url={page.url!r}",
            )
            for post in chat_posts:
                self.assertNotIn(
                    "provider='openai'",
                    post,
                    f"Send after OpenAI pick must not POST provider=openai: "
                    f"{chat_posts!r}",
                )
            _assert_no_openai_side_effects("new chat send")
            self.assertRegex(
                page.url.rstrip("/"),
                r"/chat$",
                f"aborted first send must stay on New chat: {page.url}",
            )

            planted = _json_ok(
                page.request.post(
                    f"{UI}/api/workspaces/{ws}/chat/conversations",
                    data=json.dumps(
                        {
                            "title": f"planted-oai-{uuid.uuid4().hex[:8]}",
                            "provider": first_available,
                            "model": ready_model,
                        }
                    ),
                    headers={"Content-Type": "application/json"},
                )
            )
            conv = planted.get("conversation") or planted
            cid = str(conv.get("id") or "")
            self.assertTrue(cid, f"plant conversation missing id: {planted!r}"[:400])
            self.assertNotEqual(
                str(conv.get("provider") or "").lower(),
                "openai",
                f"planted thread must not start as openai: {planted!r}"[:400],
            )

            _open_chat(f"/chat/{cid}")
            _wait_composer_ready()
            _pick_provider(first_available)
            page.wait_for_function(
                """(expected) => {
                    const s = document.querySelector("select[title='Chat provider']");
                    return !!(s && s.value === expected);
                }""",
                arg=first_available,
                timeout=8_000,
            )
            _assert_openai_pick_snaps("existing thread")
            stored = _json_ok(
                page.request.get(
                    f"{UI}/api/workspaces/{ws}/chat/conversations/{cid}"
                )
            )
            thread = stored.get("conversation") or stored
            self.assertNotEqual(
                str(thread.get("provider") or "").lower(),
                "openai",
                f"existing thread must not persist openai: {thread!r}"[:400],
            )
            for msg in stored.get("messages") or []:
                self.assertNotEqual(
                    str((msg or {}).get("provider") or "").lower(),
                    "openai",
                    f"existing thread must not persist an openai message: {msg!r}",
                )
            self.assertIn(
                cid,
                page.url,
                f"must stay on /chat/{cid}: {page.url}",
            )
        finally:
            page.unroute(_OPENAI_URL_RE, _gate)
            page.unroute(_CHAT_TURN_POST_RE, _gate)

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
