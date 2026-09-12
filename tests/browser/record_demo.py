#!/usr/bin/env python3
"""Agent-recorded product demos (Playwright .webm).

Humans do not screen-record. The coding agent runs this (usually via
./scripts/human-test.sh record) on the final tree that will be merged.
Default scenes come from HUMAN_TEST_SCOPE.md (surface_scope.py). Film
every surface that merge can break, not a random walkthrough.

Each scene must *use* the feature (click, create, cancel, save, switch,
tick) and snapshot the outcome. Heading presence is not success. After
the clip, review every mid-action frame + the .webm for bugs, fill the
Film review section in HUMAN_TEST_SCOPE.md, and attach the .webm to the
GitLab MR. The recorder exits non-zero when it found a product bug.

  python3 tests/browser/record_demo.py --list
  python3 tests/browser/record_demo.py              # HUMAN_TEST_SCOPE.md
  python3 tests/browser/record_demo.py fleet chat
  python3 tests/browser/record_demo.py --out .tmp/demos/custom

Requires this-tree UI + API (FLEET_UI_URL / FLEET_API_URL, else :3030/:8080).
Fail-closed if the stack or Playwright is missing.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import time
import uuid
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable, Iterator
from urllib.parse import urlparse
from urllib.request import urlopen

sys.path.insert(0, str(Path(__file__).resolve().parent))
from stack_identity import ForeignStackError, require_this_tree  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
UI = os.environ.get("FLEET_UI_URL", "http://localhost:3030").rstrip("/")
API = os.environ.get("FLEET_API_URL", "http://127.0.0.1:8080").rstrip("/")
BROWSERS = os.environ.get(
    "PLAYWRIGHT_BROWSERS_PATH",
    str(REPO / ".tmp" / "ms-playwright"),
)
os.environ.setdefault("PLAYWRIGHT_BROWSERS_PATH", BROWSERS)

DEMO_PASSWORD = "demo-pass-ok"
PAUSE_MS = int(os.environ.get("FLEET_DEMO_PAUSE_MS", "700"))
SERVER_LOG = REPO / ".run" / "logs" / "fleet-server.log"


def log_mark(path: Path = SERVER_LOG) -> int:
    return path.stat().st_size if path.is_file() else 0


def log_since(mark: int, path: Path = SERVER_LOG) -> str:
    if not path.is_file():
        return ""
    with path.open("r", encoding="utf-8", errors="replace") as fh:
        fh.seek(mark)
        return fh.read()


def match_http_log(
    chunk: str, method: str, uri: str, statuses: tuple[str, ...]
) -> str | None:
    """Find a TraceLayer line for this request in a log tail."""
    for line in chunk.splitlines():
        if f"method={method}" not in line:
            continue
        if uri not in line:
            continue
        if any(f"status={s}" in line for s in statuses):
            return line
    return None


def match_phrase_log(chunk: str, phrase: str) -> str | None:
    for line in chunk.splitlines():
        if phrase in line:
            return line
    return None


def _listening(host: str, port: int) -> bool:
    try:
        with socket.create_connection((host, port), timeout=2):
            return True
    except OSError:
        return False


def _ui_listen_addr() -> tuple[str, int]:
    parsed = urlparse(UI)
    host = parsed.hostname or "127.0.0.1"
    if host == "localhost":
        host = "127.0.0.1"
    port = parsed.port or (443 if parsed.scheme == "https" else 80)
    return host, port


def require_stack() -> None:
    try:
        require_this_tree(REPO, API)
    except ForeignStackError as e:
        raise SystemExit(str(e)) from e
    host, port = _ui_listen_addr()
    if not _listening(host, port):
        raise SystemExit(
            f"UI {UI} is down. Start with ./scripts/human-test.sh launch "
            "(needs dx / Dioxus CLI)."
        )


def require_playwright():
    try:
        from playwright.sync_api import sync_playwright
    except ImportError as e:
        raise SystemExit(
            "playwright is not installed. Run "
            "crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh "
            "with ENABLE_BROWSER_E2E=1 (also installs Chromium OS libraries)."
        ) from e
    return sync_playwright


def pause(page, ms: int = PAUSE_MS) -> None:
    page.wait_for_timeout(ms)


def expand_workspace_nav(page) -> None:
    # Workspace accordion starts collapsed.
    ws = page.get_by_text("Workspace", exact=True).first
    if ws.count():
        ws.click()
        pause(page, 400)


def expand_agents_nav(page) -> None:
    agents = page.get_by_text("Agents", exact=True).first
    if agents.count():
        agents.click()
        pause(page, 400)


def expand_runners_nav(page) -> None:
    runners = page.get_by_text("Runners", exact=True).first
    if runners.count():
        runners.click()
        pause(page, 400)


class Demo:
    def __init__(self, page, out_dir: Path, frames_dir: Path | None = None):
        self.page = page
        self.out_dir = out_dir
        self.frames_dir = frames_dir
        self.email = f"demo.{uuid.uuid4().hex[:10]}@example.test"
        self.workspace_url: str | None = None
        self.home_workspace_url: str | None = None
        self.home_workspace_name: str | None = None
        self.scene = ""
        self._shot = 0
        self.findings: list[str] = []

    def snap(self, label: str) -> Path | None:
        """Screenshot the current state. Mid-action frames are how we review."""
        if self.frames_dir is None:
            return None
        self._shot += 1
        scene = self.scene or "scene"
        path = self.frames_dir / f"{self._shot:02d}-{scene}-{label}.png"
        self.page.screenshot(path=str(path), full_page=True)
        print(f"FRAME {path}", flush=True)
        return path

    def bug(self, msg: str) -> None:
        line = f"{self.scene or '?'}: {msg}"
        self.findings.append(line)
        print(f"BUG {line}", flush=True)

    def dismiss_toasts(self) -> None:
        p = self.page
        for _ in range(8):
            oks = p.get_by_role("button", name="OK", exact=True)
            clicked = False
            for i in range(oks.count()):
                btn = oks.nth(i)
                try:
                    if btn.is_visible():
                        # force: the toast card used to intercept its own OK
                        # and nearby controls (Apply).
                        btn.click(timeout=1000, force=True)
                        clicked = True
                except Exception:
                    continue
            if not clicked:
                return
            pause(p, 200)

    def note_unexpected_errors(self, allowed: tuple[str, ...] = ()) -> None:
        """Flag red error copy that is not in the allowed list for this scene."""
        body = self.page.inner_text("body")
        for line in body.splitlines():
            low = line.strip()
            if not low:
                continue
            if any(a.lower() in low.lower() for a in allowed):
                continue
            if re.search(
                r"\b(failed|error|traceback|panic|unhandled|reconnect)\b",
                low,
                re.I,
            ) and not re.search(
                r"not installed|coming soon|placeholder|no models",
                low,
                re.I,
            ):
                # Short product error lines only — skip long JSON dumps.
                if 12 < len(low) < 180:
                    self.bug(f"error copy on screen: {low}")

    def assert_readable_control(self, locator, name: str) -> None:
        """Accessible name is not enough — white-on-white is a bug."""
        if locator.count() == 0:
            raise RuntimeError(f"missing control {name!r}")
        info = locator.first.evaluate(
            """el => {
                const s = getComputedStyle(el);
                const parse = (c) => {
                    const m = String(c).match(/rgba?\\((\\d+),\\s*(\\d+),\\s*(\\d+)/);
                    return m ? [+m[1], +m[2], +m[3]] : null;
                };
                const lum = (rgb) => 0.2126*rgb[0] + 0.7152*rgb[1] + 0.0722*rgb[2];
                const fg = parse(s.color);
                const bg = parse(s.backgroundColor);
                let contrast = null;
                if (fg && bg) contrast = Math.abs(lum(fg) - lum(bg));
                const r = el.getBoundingClientRect();
                return {
                    text: (el.innerText || '').trim(),
                    color: s.color,
                    bg: s.backgroundColor,
                    contrast,
                    w: r.width,
                    h: r.height,
                    disabled: !!el.disabled,
                };
            }"""
        )
        if not info.get("text"):
            self.bug(f"{name} has no visible label (blank control)")
        contrast = info.get("contrast")
        if contrast is not None and contrast < 50:
            self.bug(
                f"{name} unreadable contrast fg={info.get('color')} "
                f"bg={info.get('bg')} delta={contrast:.1f}"
            )
        if info.get("w", 0) < 8 or info.get("h", 0) < 8:
            self.bug(f"{name} collapsed ({info.get('w')}x{info.get('h')})")

    @contextmanager
    def expect_http(
        self,
        method: str,
        uri: str,
        status: str | tuple[str, ...] = "200",
        phrase: str | None = None,
        timeout_s: float = 12.0,
    ) -> Iterator[None]:
        """Click inside this block; the server log must show the request.

        `uri` is a substring (e.g. `/fleet/tick`). Status is the TraceLayer
        `status=200` token. `phrase` is an extra domain line (signup succeeded).
        """
        statuses = (status,) if isinstance(status, str) else tuple(status)
        mark = log_mark()
        yield
        deadline = time.time() + timeout_s
        chunk = ""
        http_hit: str | None = None
        phrase_hit: str | None = None
        while time.time() < deadline:
            chunk = log_since(mark)
            if http_hit is None:
                http_hit = match_http_log(chunk, method, uri, statuses)
            if phrase and phrase_hit is None:
                phrase_hit = match_phrase_log(chunk, phrase)
            if http_hit and (not phrase or phrase_hit):
                print(
                    f"LOG {method} {uri} -> {http_hit.strip()[-180:]}",
                    flush=True,
                )
                if phrase_hit:
                    print(f"LOG phrase {phrase!r}: {phrase_hit.strip()[-160:]}", flush=True)
                return
            time.sleep(0.15)
        if http_hit is None:
            other = None
            for line in chunk.splitlines():
                if f"method={method}" in line and uri in line:
                    other = line
                    break
            if other is not None:
                self.bug(
                    f"{method} {uri} logged as {other.strip()[-180:]}, "
                    f"wanted status={statuses}"
                )
            tail = chunk[-600:] if chunk else "(empty / missing .run/logs/fleet-server.log)"
            self.bug(
                f"click produced no {method} {uri} status={statuses} in server log; "
                f"tail={tail!r}"
            )
        elif phrase and phrase_hit is None:
            self.bug(
                f"{method} {uri} logged but missing phrase {phrase!r} in the same tail"
            )

    def signup_login(self) -> None:
        p = self.page
        p.goto(f"{UI}/signup", wait_until="networkidle")
        pause(p)
        self.snap("signup-form")
        p.locator("#display_name").fill("Demo Agent")
        p.locator("#email").fill(self.email)
        p.locator("#password").fill(DEMO_PASSWORD)
        pause(p)
        with self.expect_http(
            "POST", "/api/auth/signup", status="201", phrase="signup succeeded"
        ):
            p.get_by_role("button", name="Sign up").click()
        p.wait_for_url("**/login", timeout=20_000)
        if "/dashboard" in p.url:
            raise RuntimeError("signup landed inside the app instead of /login")
        pause(p)
        self.snap("after-signup-login")
        p.locator("#email").fill(self.email)
        p.locator("#password").fill(DEMO_PASSWORD)
        with self.expect_http(
            "POST", "/api/auth/login", status="200", phrase="login succeeded"
        ):
            p.get_by_role("button", name="Log in").click()
        p.wait_for_url("**/dashboard", timeout=25_000)
        p.wait_for_selector("h1")
        self.workspace_url = p.url.rsplit("/dashboard", 1)[0]
        self.home_workspace_url = self.workspace_url
        heading = p.locator("h1").first.inner_text().strip()
        if heading not in ("Dashboard", "Workforce"):
            self.bug(f"login landed on {heading!r} ({p.url})")
        aside = p.locator("aside").inner_text()
        if "Demo Agent" not in aside:
            self.bug("sidebar missing signed-in name after login")
        self.home_workspace_name = "Demo Agent's workspace"
        self.snap("after-login-dashboard")
        pause(p, 400)

    def ensure_session(self) -> str:
        if not self.workspace_url:
            self.signup_login()
        assert self.workspace_url
        return self.workspace_url

    def scene_landing(self) -> None:
        p = self.page
        p.goto(f"{UI}/", wait_until="networkidle")
        pause(p, 800)
        self.snap("hero")
        started = p.get_by_role("link", name=re.compile(r"Get started|Sign up", re.I))
        login = p.get_by_role("link", name=re.compile(r"Log in", re.I))
        if started.count() == 0:
            self.bug("landing missing Get started / Sign up")
        if login.count() == 0:
            self.bug("landing missing Log in")
        if started.count():
            started.first.click()
            pause(p, 600)
            if "/signup" not in p.url and "/login" not in p.url:
                self.bug(f"Get started went to {p.url}")
            self.snap("get-started")

    def scene_auth(self) -> None:
        # Scope order may film other pages first. Reuse that session;
        # a second Sign up with the same email never reaches /login.
        if not self.workspace_url:
            self.signup_login()
        p = self.page
        p.goto(self.workspace_url + "/dashboard", wait_until="networkidle")
        pause(p, 600)
        p.reload(wait_until="networkidle")
        pause(p, 600)
        if "/login" in p.url:
            raise RuntimeError("session cookie did not survive reload")
        self.snap("reload-still-in")

        # Login wall: a cookieless context must bounce a deep link to /login.
        browser = p.context.browser
        if browser is not None:
            wall = browser.new_context(base_url=UI, viewport={"width": 1400, "height": 900})
            try:
                other = wall.new_page()
                other.set_default_timeout(15_000)
                other.goto(self.workspace_url + "/dashboard", wait_until="domcontentloaded")
                other.wait_for_url("**/login", timeout=15_000)
                if "/login" not in other.url:
                    self.bug(f"unauth deep link stayed on {other.url}")
            except Exception as e:
                self.bug(f"unauth deep link did not redirect to /login: {e}")
            finally:
                wall.close()

    def scene_shell(self) -> None:
        base = self.ensure_session()
        p = self.page
        p.goto(f"{base}/dashboard", wait_until="networkidle")
        pause(p)
        self.snap("dashboard")
        for label in ("Fleet", "Sandboxes", "Cases", "Inbox", "New chat"):
            link = p.get_by_role("link", name=label, exact=True)
            if link.count() == 0:
                self.bug(f"sidebar missing {label}")
                continue
            link.first.click()
            pause(p, 500)
            self.snap(f"nav-{label.lower().replace(' ', '-')}")
        expand_agents_nav(p)
        p.get_by_role("link", name="All agents", exact=True).first.click()
        pause(p)
        self.snap("nav-agents")
        expand_workspace_nav(p)
        p.get_by_role("link", name="Integrations", exact=True).first.click()
        pause(p, 600)
        self.snap("nav-integrations")
        menu = p.locator("button[aria-haspopup='menu']")
        menu.click()
        pause(p, 500)
        self.snap("user-menu")
        appearance = p.get_by_role("button", name=re.compile(r"Appearance"))
        if appearance.count() == 0:
            self.bug("user menu missing Appearance / theme toggle")
        else:
            before = p.evaluate(
                """() => {
                    const el = document.querySelector('[class*="theme-"]');
                    return el ? el.className : '';
                }"""
            )
            appearance.first.click()
            pause(p, 700)
            after = p.evaluate(
                """() => {
                    const el = document.querySelector('[class*="theme-"]');
                    return el ? el.className : '';
                }"""
            )
            self.snap("theme-toggled")
            if before == after:
                self.bug(f"theme toggle did not change class ({before!r})")
            p.keyboard.press("Escape")
            pause(p, 400)
            if p.locator("#user-menu-dismiss").count():
                try:
                    p.locator("#user-menu-dismiss").click(position={"x": 8, "y": 8})
                    pause(p, 300)
                except Exception:
                    pass
            try:
                p.locator("button[aria-haspopup='menu']").first.click()
                pause(p, 300)
                again = p.get_by_role("button", name=re.compile(r"Appearance"))
                if again.count():
                    again.first.click()
                    pause(p, 400)
                    self.snap("theme-restored")
                p.keyboard.press("Escape")
            except Exception as e:
                self.bug(f"could not restore theme: {e}")
                p.keyboard.press("Escape")

    def scene_workspace(self) -> None:
        base = self.ensure_session()
        p = self.page
        p.goto(f"{base}/dashboard", wait_until="networkidle")
        pause(p)
        first = p.url
        p.locator("aside").get_by_text("▼").first.click()
        pause(p, 400)
        self.snap("switcher-open")
        p.get_by_role("button", name="+ Create workspace").click()
        pause(p, 400)
        created_name = f"Demo ws {uuid.uuid4().hex[:6]}"
        p.get_by_placeholder("Workspace name").fill(created_name)
        pause(p)
        with self.expect_http("POST", "/api/workspaces version", status="201"):
            p.get_by_role("button", name="Create", exact=True).click()
        p.wait_for_url("**/dashboard", timeout=15_000)
        pause(p, 800)
        if p.url == first:
            self.bug("create workspace did not change the dashboard URL")
        self.workspace_url = p.url.rsplit("/dashboard", 1)[0]
        self.snap("created-second")

        # Switch back to the first workspace — that's the feature.
        p.locator("aside").get_by_text("▼").first.click()
        pause(p, 400)
        home = self.home_workspace_name or "Demo Agent's workspace"
        target = p.locator("aside").get_by_text(home, exact=False)
        if target.count() == 0:
            self.bug(f"switcher missing first workspace {home!r}")
        else:
            target.first.click()
            pause(p, 800)
            if self.home_workspace_url and self.home_workspace_url not in p.url:
                self.bug(
                    f"switch back stayed on {p.url}, expected {self.home_workspace_url}"
                )
            else:
                self.workspace_url = p.url.rsplit("/dashboard", 1)[0]
            self.snap("switched-home")

    def scene_api(self) -> None:
        p = self.page
        p.goto(f"{API}/health", wait_until="domcontentloaded")
        pause(p, 800)
        body = p.inner_text("body")
        self.snap("health")
        if "fleet-server" not in body and "ok" not in body:
            raise RuntimeError(f"unexpected /health body: {body[:300]}")
        if "chat" not in body:
            raise RuntimeError(f"/health missing chat in apis (old server): {body[:400]}")
        try:
            data = json.loads(body)
        except json.JSONDecodeError:
            data = {}
        tree = subprocess.check_output(
            [str(REPO / "scripts" / "tree-id.sh")],
            cwd=str(REPO),
            text=True,
        ).strip()
        sha = str(data.get("git_sha") or "")
        if sha and sha != tree:
            self.bug(f"/health git_sha {sha} != tree-id {tree}")
        p.goto(f"{API}/", wait_until="domcontentloaded")
        pause(p, 800)
        self.snap("index")

    def scene_chat(self) -> None:
        base = self.ensure_session()
        p = self.page
        p.goto(f"{base}/chat", wait_until="networkidle")
        pause(p, 1000)
        body = p.inner_text("body")
        if "Chat backend not connected yet." in body:
            raise RuntimeError("stub chat UI — not this tree's fleet-ui")
        if "Pick the runner by hand" not in body and "Select runner" not in body:
            raise RuntimeError("chat composer missing runner picker")
        self.snap("composer")

        # Try to pick a live provider before giving up.
        provider = p.get_by_label(re.compile(r"provider", re.I))
        if provider.count() == 0:
            provider = p.locator("select").nth(1) if p.locator("select").count() > 1 else p.locator("select")
        if provider.count():
            try:
                options = provider.first.locator("option").all_inner_texts()
                live = next(
                    (
                        o
                        for o in options
                        if "not installed" not in o.lower() and o.strip()
                    ),
                    None,
                )
                if live:
                    provider.first.select_option(label=live)
                    pause(p, 600)
                    self.snap("provider-switched")
            except Exception:
                # Composer often uses a custom listbox, not a <select>.
                pass

        box = p.locator("textarea")
        send = p.get_by_role("button", name="Send message")
        if box.count() == 0:
            self.bug("chat missing composer textarea")
            return
        if not box.first.is_enabled():
            # Disabled is honest when no CLI/provider/runner is ready.
            lowered = body.lower()
            if not any(
                n in lowered
                for n in (
                    "not installed",
                    "required",
                    "select runner",
                    "select a runner",
                    "pick the runner",
                )
            ):
                self.bug("composer disabled without an offline reason")
            self.snap("composer-disabled")
            return
        ping = f"Human-test ping {uuid.uuid4().hex[:6]}"
        box.first.fill(ping)
        pause(p, 400)
        if send.count() and send.first.is_enabled():
            with self.expect_http(
                "POST", "/chat", status=("200", "201", "400", "422")
            ):
                send.first.click()
            pause(p, 2500)
            after = p.inner_text("body")
            if ping not in after:
                self.bug("sent chat text never appeared as a bubble")
            self.snap("after-send")
        else:
            self.snap("send-disabled-after-fill")

    def scene_fleet(self) -> None:
        base = self.ensure_session()
        p = self.page
        p.goto(f"{base}/fleet", wait_until="networkidle")
        pause(p, 1000)
        if p.locator("h1").first.inner_text().strip() != "Fleet":
            raise RuntimeError("Fleet page missing Fleet heading")
        start = p.get_by_role("button", name="Start 2-task run")
        if start.count() == 0:
            raise RuntimeError("Fleet page missing 'Start 2-task run'")
        self.assert_readable_control(start, "Start 2-task run")
        self.assert_readable_control(
            p.get_by_role("button", name="Run tick"), "Run tick"
        )
        self.snap("before-start")

        before_body = p.inner_text("body")
        with self.expect_http("POST", "/fleet/runs/fixture", status=("200", "201")):
            start.first.click()
        try:
            p.get_by_text(re.compile(r"Started run"), exact=False).wait_for(
                timeout=15_000
            )
        except Exception:
            self.bug("Start 2-task run did not show 'Started run'")
        pause(p, 800)
        self.snap("after-start")
        after_start = p.inner_text("body")
        if "Started run" not in after_start and "Root task" not in after_start:
            self.bug("Start 2-task run left no run id or board cards")

        tick = p.get_by_role("button", name="Run tick")
        if tick.count() == 0:
            self.bug("Run tick missing after start")
        else:
            with self.expect_http("POST", "/fleet/tick", status="200"):
                tick.first.click()
            pause(p, 1500)
            self.snap("after-tick-1")
            with self.expect_http("POST", "/fleet/tick", status="200"):
                tick.first.click()
            pause(p, 1500)
            self.snap("after-tick-2")
        ticked = p.inner_text("body")
        if "Tick ·" not in ticked and "tick" not in ticked.lower():
            self.bug("Run tick left no tick status")
        # Cross-surface: dashboard should reflect the run.
        p.goto(f"{base}/dashboard", wait_until="networkidle")
        pause(p, 800)
        dash = p.inner_text("body")
        self.snap("dashboard-after-run")
        if "open runs" not in dash.lower():
            self.bug("dashboard missing Open runs after fleet start")
        p.goto(f"{base}/fleet", wait_until="networkidle")
        pause(p, 600)
        cancel = p.get_by_role("button", name="Cancel latest")
        if cancel.count() and cancel.first.is_enabled():
            with self.expect_http("POST", "/cancel", status="200"):
                cancel.first.click()
            pause(p, 1200)
            self.snap("after-cancel")
        if "failed_spawn" in ticked.lower():
            self.bug("fleet tick reported failed_spawn")
        del before_body

    def scene_sandboxes(self) -> None:
        base = self.ensure_session()
        p = self.page
        p.goto(f"{base}/sandboxes", wait_until="networkidle")
        pause(p, 1000)
        if p.locator("h1").first.inner_text().strip() != "Sandboxes":
            raise RuntimeError("Sandboxes page missing heading")
        body = p.inner_text("body")
        self.snap("list")
        refresh = p.get_by_role("button", name="Refresh")
        if refresh.count():
            refresh.first.click()
            pause(p, 800)
            self.snap("after-refresh")
        if "sandbox session" not in body.lower() and "no sandbox" not in body.lower():
            # Either a count line or an empty hint is required.
            if "Fleet" not in body:
                self.bug("sandboxes page has neither sessions nor a Fleet hint")
        card = p.locator("article, li, section").filter(
            has_text=re.compile(r"session|exited|spawning|running", re.I)
        )
        if card.count():
            try:
                card.first.click()
                pause(p, 600)
                self.snap("session-clicked")
            except Exception:
                pass

    def scene_cases(self) -> None:
        base = self.ensure_session()
        p = self.page
        p.goto(f"{base}/cases", wait_until="networkidle")
        pause(p, 800)
        if p.locator("h1").first.inner_text().strip() != "Cases":
            raise RuntimeError("Cases page missing Cases heading")
        title = p.locator("input[placeholder*='Title']")
        policy = p.locator("textarea[placeholder*='Policy']")
        if title.count() == 0 or policy.count() == 0:
            raise RuntimeError("Cases page missing New case title/policy fields")
        self.snap("empty-form")
        title.first.fill("Demo case")
        policy.first.fill("Wait until observed, then stay open until cancelled.")
        create = p.get_by_role("button", name="Create case")
        if create.count() == 0:
            raise RuntimeError("Cases page missing 'Create case'")
        with self.expect_http("POST", "/cases", status=("200", "201")):
            create.first.click()
        p.wait_for_url(re.compile(r"/cases/[0-9a-f-]{36}", re.I), timeout=15_000)
        try:
            p.get_by_text("Case created.", exact=True).wait_for(timeout=10_000)
        except Exception:
            self.bug("create case produced no success toast")
        self.dismiss_toasts()
        pause(p, 600)
        self.snap("created")
        open_body = p.inner_text("body")
        if "Demo case" not in open_body:
            self.bug("created case title missing on detail")
        for action in ("Observe", "Decide now", "Complete", "Cancel"):
            if p.get_by_role("button", name=action, exact=True).count() == 0:
                self.bug(f"open case missing {action}")

        note = p.locator("input[placeholder*='Observe']")
        if note.count() == 0:
            raise RuntimeError("Case detail missing Observe field")
        note.first.fill("Demo observed a reply in Slack.")
        observe = p.get_by_role("button", name="Observe", exact=True)
        with self.expect_http("POST", "/observe", status="200"):
            observe.first.click()
        try:
            p.get_by_text("Observation recorded.", exact=True).wait_for(timeout=10_000)
        except Exception:
            self.bug("Observe produced no success toast")
        self.dismiss_toasts()
        pause(p, 600)
        self.snap("observed")
        if "observed" not in p.inner_text("body").lower():
            self.bug("events list missing observed after Observe")

        cancel = p.get_by_role("button", name="Cancel", exact=True)
        if cancel.count() == 0:
            raise RuntimeError("Case detail missing Cancel")
        with self.expect_http("POST", "/cases/", status="200"):
            cancel.first.click()
        try:
            p.get_by_text("Case cancelled.", exact=True).wait_for(timeout=10_000)
        except Exception:
            self.bug("Cancel produced no success toast")
        self.dismiss_toasts()
        pause(p, 800)
        self.snap("cancelled")
        detail = p.inner_text("body")
        if "cancelled" not in detail.lower():
            self.bug("cancelled case does not show cancelled status")
        # Uppercase mash of status+stage ("CANCELLED - START") is unreadable.
        mashed = p.locator("span").filter(
            has_text=re.compile(r"cancelled\s*[-·]\s*start", re.I)
        )
        if mashed.count():
            self.bug(
                "case detail pill mashes status and stage "
                f"({mashed.first.inner_text()!r})"
            )
        if p.get_by_role("button", name="Observe", exact=True).count():
            # Terminal cases should hide open actions.
            if p.get_by_role("button", name="Observe", exact=True).first.is_enabled():
                self.bug("Observe still enabled after cancel")

    def scene_inbox(self) -> None:
        from inbox_e2e_helpers import seed_open_ticket

        base = self.ensure_session()
        p = self.page
        p.goto(f"{base}/inbox", wait_until="networkidle")
        pause(p, 800)
        if p.locator("h1").first.inner_text().strip() != "Inbox":
            raise RuntimeError("Inbox page missing Inbox heading")
        for name in ("Start monitor", "Stop monitor", "Refresh now"):
            if p.get_by_role("button", name=name).count() == 0:
                raise RuntimeError(f"Inbox page missing {name}")
        self.snap("empty")
        ws = base.rstrip("/").rsplit("/", 1)[-1]
        # Ticket only — do not fake a GitHub connection. A user without
        # OAuth still has Finish; Start monitor should say to connect.
        key = f"demo-inbox-{uuid.uuid4().hex[:8]}/board#1"
        title = "Demo inbox ticket"
        seed_open_ticket(ws, "github", key, title)
        start = p.get_by_role("button", name="Start monitor")
        with self.expect_http("POST", "/inbox version", status=("200", "400", "422")):
            start.first.click()
        pause(p, 1500)
        self.snap("after-start-monitor")
        body = p.inner_text("body")
        if "access_token" in body.lower() or "reconnect" in body.lower():
            self.bug("inbox shows reconnect/access_token with no connected app")
        if "monitor on" not in body.lower():
            # Expected when no integration is connected.
            if "connect" not in body.lower() and "integration" not in body.lower():
                self.bug("Start monitor neither enabled nor explained connect-first")
        refresh = p.get_by_role("button", name="Refresh now")
        with self.expect_http("POST", "/inbox/poll", status=("200", "400", "422")):
            refresh.first.click()
        pause(p, 1200)
        p.get_by_text(f"{key} · {title}", exact=False).first.wait_for(timeout=15_000)
        self.dismiss_toasts()
        self.snap("ticket-visible")
        finish = p.get_by_role("button", name="Finish", exact=True)
        if finish.count() == 0:
            raise RuntimeError("Inbox row missing Finish")
        with self.expect_http("POST", "/inbox/finish", status=("200", "201")):
            finish.first.click()
        pause(p, 1500)
        after = p.inner_text("body")
        if "launched" not in after.lower() and "run " not in after.lower():
            try:
                p.get_by_text(re.compile(r"Fleet launched|launched"), exact=False).wait_for(
                    timeout=10_000
                )
                after = p.inner_text("body")
            except Exception:
                self.bug("Finish did not leave a launched row or toast")
        self.dismiss_toasts()
        self.snap("after-finish")
        if p.get_by_role("button", name="Finish", exact=True).count():
            # Finish should hide once the row is launched.
            if p.get_by_role("button", name="Finish", exact=True).first.is_visible():
                self.bug("Finish still visible after launch")
        if "failed_spawn" in after.lower():
            self.bug("Inbox Finish reported failed_spawn")

    def scene_agents(self) -> None:
        base = self.ensure_session()
        p = self.page
        p.goto(f"{base}/agents", wait_until="networkidle")
        pause(p, 800)
        if p.locator("h1").first.inner_text().strip() != "Agents":
            raise RuntimeError("Agents page missing heading")
        self.snap("before-seed")
        seed = p.get_by_role("button", name="Seed starters")
        if seed.count() == 0:
            self.bug("Agents missing Seed starters")
        else:
            with self.expect_http("POST", "/personas/seed-starters", status="200"):
                seed.first.click()
            try:
                p.get_by_text(re.compile(r"Seeded \d+ starter"), exact=False).wait_for(
                    timeout=15_000
                )
            except Exception:
                self.bug("Seed starters produced no toast")
            pause(p, 800)
            self.dismiss_toasts()
            self.snap("after-seed")
        starters = p.get_by_text("STARTER", exact=False)
        if starters.count() < 4:
            self.bug(f"expected 8 starter personas, visible STARTER marks={starters.count()}")
        name = p.get_by_placeholder("Name")
        desc = p.get_by_placeholder("Short description")
        prompt = p.get_by_placeholder("Role prompt")
        if name.count() == 0:
            self.bug("Agents missing create-persona form")
        else:
            custom = f"Demo persona {uuid.uuid4().hex[:6]}"
            name.first.fill(custom)
            if desc.count():
                desc.first.fill("Created by the film to prove create works.")
            if prompt.count():
                prompt.first.fill("You are a demo persona. Do nothing harmful.")
            create = p.get_by_role("button", name="Create persona")
            if create.count() == 0:
                self.bug("missing Create persona")
            else:
                with self.expect_http("POST", "/personas version", status=("200", "201")):
                    create.first.click()
                try:
                    p.get_by_text("Persona created.", exact=True).wait_for(timeout=10_000)
                except Exception:
                    self.bug("Create persona produced no toast")
                pause(p, 600)
                self.dismiss_toasts()
                self.snap("persona-created")
                if custom not in p.inner_text("body"):
                    self.bug(f"created persona {custom!r} not listed")

    def scene_integrations(self) -> None:
        base = self.ensure_session()
        p = self.page
        p.goto(f"{base}/integrations", wait_until="networkidle")
        pause(p, 800)
        if p.locator("h1").first.inner_text().strip() != "Integrations":
            raise RuntimeError("Integrations page missing heading")
        search = p.locator("input[type='search'], input[placeholder*='Search' i]")
        if search.count() == 0:
            self.bug("Integrations missing search")
            self.snap("catalog")
            return
        search.first.fill("git")
        pause(p, 800)
        self.snap("search-git")
        git_body = p.inner_text("body")
        if "GitHub" not in git_body and "github" not in git_body.lower():
            self.bug("search git did not show GitHub")
        connect = p.get_by_role("button", name="Connect", exact=True)
        if connect.count():
            # Click Connect — do not complete OAuth. A popup, a same-tab
            # redirect, or an on-page waiting state is the feature.
            try:
                with p.expect_popup(timeout=4000) as pop:
                    with self.expect_http(
                        "GET",
                        "/oauth/start",
                        status=("200", "302", "303", "307", "400"),
                    ):
                        connect.first.click()
                popup = pop.value
                pause(popup, 800)
                self.snap("connect-popup")
                popup.close()
            except Exception:
                pause(p, 1000)
            if "/oauth" in p.url or "authorize" in p.url.lower():
                self.snap("oauth-redirect")
                p.goto(f"{base}/integrations", wait_until="networkidle")
                pause(p, 600)
            else:
                self.snap("after-connect-click")
        search = p.locator("input[type='search'], input[placeholder*='Search' i]")
        if search.count():
            search.first.fill("asana")
            pause(p, 800)
            self.snap("search-asana")
            if "Asana" not in p.inner_text("body"):
                self.bug("search asana did not show Asana")

    def scene_admin(self) -> None:
        """Instance admin: counts, users, promote/demote, runners, health."""
        import subprocess

        base = self.ensure_session()
        p = self.page
        url = os.environ.get("DATABASE_URL", "").strip()
        if not url:
            envp = REPO / ".env"
            if envp.is_file():
                for line in envp.read_text(encoding="utf-8").splitlines():
                    if line.startswith("DATABASE_URL="):
                        url = line.split("=", 1)[1].strip().strip('"').strip("'")
                        break
        if not url:
            raise RuntimeError("DATABASE_URL required to promote the demo instance admin")
        sql = (
            "UPDATE users SET instance_admin = TRUE, updated_at = now() "
            f"WHERE email_normalized = lower(trim('{self.email}')) RETURNING email;"
        )
        out = subprocess.check_output(
            ["psql", url, "-v", "ON_ERROR_STOP=1", "-tAc", sql],
            text=True,
        )
        if self.email.lower() not in out.lower():
            raise RuntimeError(f"promote missed {self.email}: {out!r}")

        p.goto(f"{UI}/admin", wait_until="domcontentloaded")
        p.get_by_role("heading", name="Instance", exact=True).first.wait_for(
            timeout=25_000
        )
        p.get_by_role("heading", name="Users", exact=True).wait_for(timeout=25_000)
        pause(p, 800)
        if p.locator("h1").first.inner_text().strip() != "Instance":
            raise RuntimeError(f"admin missing Instance heading ({p.url})")
        self.snap("loaded")
        if p.get_by_role("heading", name="Users", exact=True).count() == 0:
            raise RuntimeError("admin missing Users")
        if p.get_by_text(self.email, exact=False).count() == 0:
            self.bug(f"admin users missing signed-in {self.email}")

        guest = f"demo-admin-{uuid.uuid4().hex[:8]}@example.test"
        res = p.request.post(
            f"{UI}/api/auth/signup",
            data=json.dumps(
                {
                    "email": guest,
                    "password": DEMO_PASSWORD,
                    "display_name": "Demo Guest",
                }
            ),
            headers={"content-type": "application/json"},
        )
        if res.status != 201:
            self.bug(f"guest signup {res.status}: {res.text()[:200]}")
        else:
            p.reload(wait_until="networkidle")
            pause(p, 600)
            row = p.locator("tr").filter(has_text=guest)
            if row.count() == 0:
                self.bug(f"guest {guest} missing from users after reload")
            else:
                row.first.scroll_into_view_if_needed()
                with self.expect_http("PATCH", "/api/admin/users/", status="200"):
                    row.get_by_role("button", name="Promote", exact=True).click()
                try:
                    p.get_by_text("Promoted to instance admin.", exact=True).wait_for(
                        timeout=10_000
                    )
                except Exception:
                    self.bug("Promote produced no toast")
                self.dismiss_toasts()
                self.snap("promoted")
                row = p.locator("tr").filter(has_text=guest)
                with self.expect_http("PATCH", "/api/admin/users/", status="200"):
                    row.get_by_role("button", name="Demote", exact=True).click()
                try:
                    p.get_by_text("Demoted instance admin.", exact=True).wait_for(
                        timeout=10_000
                    )
                except Exception:
                    self.bug("Demote produced no toast")
                self.dismiss_toasts()
                self.snap("demoted")

        for heading in ("Workspaces", "Runners", "Live work", "Control plane"):
            if p.get_by_role("heading", name=heading, exact=True).count() == 0:
                self.bug(f"admin missing {heading} section")
        p.get_by_role("heading", name="Runners", exact=True).first.scroll_into_view_if_needed()
        pause(p, 400)
        self.snap("runners")
        p.get_by_role("heading", name="Control plane", exact=True).first.scroll_into_view_if_needed()
        pause(p, 400)
        self.snap("health")
        self.note_unexpected_errors(
            allowed=(
                "Instance admin required",
                "No runners registered",
                "No staged worker packages",
                "failed",
                "failed_spawn",
            )
        )
        _ = base

    def scene_settings(self) -> None:
        base = self.ensure_session()
        p = self.page
        p.goto(f"{base}/settings", wait_until="networkidle")
        pause(p, 800)
        if p.locator("h1").first.inner_text().strip() != "Settings":
            raise RuntimeError("Settings page missing Settings heading")
        self.snap("loaded")
        agents = p.locator("input[type='number']")
        if agents.count() == 0:
            self.bug("Settings missing Number of agents input")
        else:
            agents.first.fill("5")
            save = p.get_by_role("button", name="Save", exact=True)
            if save.count() == 0:
                self.bug("Settings missing Save")
            else:
                with self.expect_http("PATCH", "/fleet/settings", status="200"):
                    save.first.click()
                try:
                    p.get_by_text("Settings saved.", exact=True).wait_for(
                        timeout=10_000
                    )
                except Exception:
                    self.bug("Save produced no 'Settings saved.' toast")
                self.dismiss_toasts()
                pause(p, 400)
                self.snap("saved-agents")
                p.reload(wait_until="networkidle")
                pause(p, 800)
                agents = p.locator("input[type='number']")
                got = agents.first.input_value() if agents.count() else ""
                if got != "5":
                    self.bug(f"agent count did not persist after Save+reload (got {got!r})")
                self.snap("reloaded-agents")

        heading = p.get_by_role("heading", name="Members", exact=True)
        if heading.count() == 0:
            raise RuntimeError("Settings page missing Members heading")
        heading.first.scroll_into_view_if_needed()
        pause(p, 400)
        email_box = p.locator("input[type='email'][placeholder='teammate@example.test']")
        if email_box.count() == 0:
            raise RuntimeError("Settings members missing invite email field")
        form = email_box.first.locator("xpath=..")
        role_select = form.locator("select")
        invite_btn = form.get_by_role("button", name="Invite", exact=True)
        if role_select.count() == 0 or invite_btn.count() == 0:
            raise RuntimeError("Settings members missing invite role/Invite")
        guest = f"demo-member-{uuid.uuid4().hex[:8]}@example.test"
        email_box.first.fill(guest)
        role_select.first.select_option("member")
        with self.expect_http("POST", "/invites", status="201"):
            invite_btn.first.click()
        p.get_by_text(
            "Invite sent — they join on signup or login.", exact=True
        ).wait_for(timeout=15_000)
        row = p.get_by_text(f"{guest} · member (pending)", exact=False)
        row.first.wait_for(timeout=15_000)
        row.first.scroll_into_view_if_needed()
        self.dismiss_toasts()
        pause(p, 600)
        self.snap("invite-pending")
        revoke = p.locator("li").filter(has_text=guest).get_by_role(
            "button", name="Revoke", exact=True
        )
        if revoke.count() == 0:
            self.bug("pending invite missing Revoke")
        else:
            with self.expect_http("DELETE", "/invites/", status="200"):
                revoke.first.click()
            pause(p, 1000)
            if p.get_by_text(guest, exact=False).count():
                # row may linger one paint
                pause(p, 800)
            if p.get_by_text(f"{guest} · member (pending)", exact=False).count():
                self.bug("Revoke left the pending invite on the list")
            self.dismiss_toasts()
            self.snap("invite-revoked")

    def scene_unify(self) -> None:
        """Isolation + runners + fleet tick — the unify-runners user path."""
        base = self.ensure_session()
        p = self.page
        expand_workspace_nav(p)
        p.goto(f"{base}/settings", wait_until="networkidle")
        pause(p, 700)
        self.snap("settings")
        iso = p.locator("select").filter(has=p.locator('option[value="container"]'))
        if iso.count():
            iso.first.select_option("container")
            pause(p, 400)
            save = p.get_by_role("button", name="Save")
            if save.count():
                with self.expect_http("PATCH", "/fleet/settings", status="200"):
                    save.first.click()
                pause(p, 800)
                self.dismiss_toasts()
                p.reload(wait_until="networkidle")
                pause(p, 600)
                iso = p.locator("select").filter(
                    has=p.locator('option[value="container"]')
                )
                if iso.count() and iso.first.input_value() != "container":
                    self.bug(
                        f"isolation did not persist (got {iso.first.input_value()!r})"
                    )
                self.snap("isolation-saved")
        expand_runners_nav(p)
        self._film_self_hosted("/runners/self-hosted")
        p.goto(f"{base}/fleet", wait_until="networkidle")
        pause(p, 600)
        start = p.get_by_role("button", name="Start 2-task run")
        if start.count():
            self.assert_readable_control(start, "Start 2-task run")
            with self.expect_http("POST", "/fleet/runs/fixture", status=("200", "201")):
                start.first.click()
            pause(p, 800)
            self.snap("fleet-started")
        tick = p.get_by_role("button", name="Run tick")
        if tick.count():
            with self.expect_http("POST", "/fleet/tick", status="200"):
                tick.first.click()
            pause(p, 1200)
            self.snap("fleet-ticked")
        p.goto(f"{base}/sandboxes", wait_until="networkidle")
        pause(p, 800)
        self.snap("sandboxes")

    def _film_self_hosted(self, rel: str) -> None:
        base = self.ensure_session()
        p = self.page
        p.goto(f"{base}{rel}", wait_until="networkidle")
        pause(p, 1000)
        if rel != "/runners/self-hosted":
            p.wait_for_url("**/runners/self-hosted", timeout=15_000)
        body = p.inner_text("body")
        heading = ""
        if p.locator("h1").count():
            heading = p.locator("h1").first.inner_text()
        if not any(
            label in heading or label in body
            for label in ("Self-hosted", "Runners", "Workers")
        ):
            raise RuntimeError(
                f"self-hosted runners page missing heading or text: {body[:300]}"
            )
        if (
            "No workers yet" not in body
            and p.locator("article").count() == 0
            and p.locator("button").filter(has_text="online").count() == 0
            and p.locator("button").filter(has_text="last seen").count() == 0
            and "pending" not in body.lower()
            and "Waiting to pair" not in body
        ):
            raise RuntimeError("self-hosted page missing empty state or cards")
        self.snap("list")
        add = p.get_by_role("button", name="Add worker")
        if add.count() == 0:
            self.bug("self-hosted missing Add worker")
        else:
            with self.expect_http("POST", "/worker-hosts", status=("200", "201")):
                add.first.click()
            pause(p, 1500)
            self.snap("pair-panel")
            pair_body = p.inner_text("body")
            if "install-worker.sh" not in pair_body and "pair" not in pair_body.lower():
                self.bug("Add worker did not show a pair/install command")
            unix_copy = (
                p.locator("pre")
                .filter(has_text="install-worker.sh")
                .locator("xpath=..")
                .get_by_role("button", name="Copy")
            )
            if unix_copy.count() and unix_copy.first.is_visible():
                unix_copy.first.click()
                pause(p, 600)
                try:
                    p.get_by_text("Command copied.", exact=True).wait_for(timeout=5_000)
                except Exception:
                    self.bug("Copy did not toast Command copied.")
                self.dismiss_toasts()
                self.snap("copied")
            done = p.get_by_role("button", name="Done")
            if done.count() and done.first.is_visible():
                done.first.click()
                pause(p, 600)
                self.snap("pair-dismissed")
        pending_card = p.locator("button").filter(has_text="Waiting to pair")
        if pending_card.count() == 0:
            pending_card = p.locator("button").filter(
                has_text=re.compile(r"\bpending\b", re.I)
            )
        card = pending_card.or_(p.locator("button").filter(has_text="last seen"))
        if card.count() and card.first.is_visible():
            card.first.click()
            pause(p, 800)
            self.snap("card-open")
        popular = p.get_by_text("Popular models")
        if popular.count() and popular.first.is_visible():
            filt = p.get_by_placeholder("Filter the current top 50")
            if filt.count() and filt.first.is_visible():
                filt.first.fill("llama")
                pause(p, 600)
                self.snap("models-filtered")
                filt.first.fill("")
                pause(p, 400)
        unlisted = p.get_by_placeholder("name or URL (llama3.2, hf.co/…)")
        if unlisted.count() and unlisted.first.is_visible():
            unlisted.first.fill("tinyllama")
            pause(p, 800)
            self.snap("unlisted-model")
            unlisted.first.fill("")
        hold = p.get_by_role("button", name="Stop taking new jobs")
        if hold.count() and hold.first.is_visible():
            hold.first.click()
            pause(p, 400)
            self.dismiss_toasts()
            apply = p.get_by_role("button", name="Apply", exact=True)
            if apply.count() and apply.first.is_visible() and apply.first.is_enabled():
                with self.expect_http("PATCH", "/worker-hosts/", status="200"):
                    apply.first.click()
                pause(p, 1000)
            self.snap("held")
            resume = p.get_by_role("button", name=re.compile(r"Take jobs again|Resume", re.I))
            if resume.count():
                resume.first.click()
                pause(p, 400)
                self.dismiss_toasts()
                apply = p.get_by_role("button", name="Apply", exact=True)
                if apply.count() and apply.first.is_visible() and apply.first.is_enabled():
                    with self.expect_http("PATCH", "/worker-hosts/", status="200"):
                        apply.first.click()
                    pause(p, 800)
                self.snap("resumed")

    def scene_runners(self) -> None:
        self._film_self_hosted("/runners/self-hosted")

    def scene_workers(self) -> None:
        """Alias: /{ws}/workers redirects to the self-hosted runners page."""
        self._film_self_hosted("/workers")

    def _film_network_runners(
        self,
        rel: str,
        heading_labels: tuple[str, ...],
        copy_labels: tuple[str, ...],
        nav_label: str,
    ) -> None:
        base = self.ensure_session()
        p = self.page
        p.goto(f"{base}/dashboard", wait_until="networkidle")
        pause(p)
        expand_runners_nav(p)
        link = p.get_by_role("link", name=nav_label, exact=True)
        if link.count():
            link.first.click()
            pause(p, 800)
        else:
            p.goto(f"{base}{rel}", wait_until="networkidle")
            pause(p, 1000)
        p.wait_for_url(f"**{rel}", timeout=15_000)
        heading = ""
        if p.locator("h1").count():
            heading = p.locator("h1").first.inner_text()
        if not any(label in heading for label in heading_labels):
            raise RuntimeError(
                f"{rel} missing heading {heading_labels}: {heading!r}"
            )
        # Prefer <main> so sidebar "Cloud" / "Partners" labels cannot false-green.
        main = p.locator("main")
        text = main.inner_text() if main.count() else p.inner_text("body")
        if not any(label in text for label in copy_labels):
            raise RuntimeError(
                f"{rel} missing expected copy {copy_labels}: {text[:400]}"
            )
        self.snap("page")
        pause(p, 400)

    def scene_cloud(self) -> None:
        self._film_network_runners(
            "/runners/cloud",
            ("Cloud",),
            (
                "AWS",
                "GCP",
                "Azure",
                "Fleet Cloud",
                "Public cloud",
                "public cloud",
                "Coming soon",
                "placeholder",
                "Placeholder",
            ),
            "Cloud",
        )

    def scene_partners(self) -> None:
        self._film_network_runners(
            "/runners/partners",
            ("Partners",),
            (
                "Trust",
                "Disclaimer",
                "unused capacity",
                "Coming soon",
                "Verified",
                "verified",
                "acknowledge",
            ),
            "Partners",
        )
        p = self.page
        ack = p.get_by_role("checkbox")
        if ack.count() and ack.first.is_visible():
            with self.expect_http("POST", "/partners/disclaimer", status="200"):
                ack.first.check()
            pause(p, 800)
            self.snap("acknowledged")
        lend = p.get_by_role("button", name=re.compile(r"Lend", re.I))
        buy = p.get_by_role("button", name=re.compile(r"Buy", re.I))
        for btn, label in ((lend, "Lend"), (buy, "Buy")):
            if btn.count() == 0:
                self.bug(f"Partners missing {label} control")
            elif btn.first.is_enabled():
                self.bug(f"Partners {label} enabled after acknowledge (must stay disabled)")
        self.snap("lend-buy-disabled")

    def scene_sessions(self) -> None:
        base = self.ensure_session()
        p = self.page
        p.goto(f"{base}/sessions", wait_until="networkidle")
        pause(p, 800)
        heading = p.locator("h1").first.inner_text().strip() if p.locator("h1").count() else ""
        if heading and "chat" not in heading.lower() and "history" not in heading.lower() and "session" not in heading.lower():
            self.bug(f"Chat history heading unexpected: {heading!r}")
        body = p.inner_text("body")
        if "worker" in body.lower() and "host" in body.lower() and "chat" not in body.lower():
            self.bug("sessions page looks like worker hosts, not chats")
        self.snap("list")
        row = p.get_by_role("link").filter(has_text=re.compile(r"chat|thread|untitled", re.I))
        if row.count():
            row.first.click()
            pause(p, 800)
            self.snap("opened")

    def scene_dashboard(self) -> None:
        base = self.ensure_session()
        p = self.page
        p.goto(f"{base}/dashboard", wait_until="networkidle")
        p.get_by_text("Workforce", exact=True).first.wait_for(timeout=15_000)
        p.get_by_text("Open runs").first.wait_for(timeout=10_000)
        p.get_by_text("Set a goal").first.wait_for(timeout=10_000)
        pause(p, 1500)
        self.snap("overview")

    def scene_lab(self) -> None:
        """Film a host protocol run (no Docker/LLM) as a local HTML page."""
        html_path = self.out_dir.resolve() / "lab-protocol.html"
        cmd = [
            sys.executable,
            str(
                REPO
                / "crates/fleet-sandbox-driver/sandbox-lab/workspace/tests"
                / "test_workspace_agent_protocol.py"
            ),
            "-v",
        ]
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
        log = (proc.stdout or "") + ("\n" + proc.stderr if proc.stderr else "")
        status = "PASS" if proc.returncode == 0 else "FAIL"
        html_path.write_text(
            "<!doctype html><meta charset=utf-8><title>lab protocol "
            + status
            + "</title><pre style='font:14px/1.4 ui-monospace,monospace;padding:24px'>"
            + _escape(log[-12000:])
            + "</pre>",
            encoding="utf-8",
        )
        if proc.returncode != 0:
            raise RuntimeError(f"lab protocol tests failed (see {html_path})")
        self.page.goto(html_path.as_uri(), wait_until="domcontentloaded")
        pause(self.page, 800)
        self.snap("protocol")

    def scene_k8s(self) -> None:
        """Operator-visible k8s path: CRD + operator + fsb list, then Fleet header (k8s in-cluster)."""
        lines: list[str] = []
        kubectl = shutil.which("kubectl")
        if kubectl:
            for args in (
                ["get", "crd", "fleetsandboxes.fleet.agentfleet.io"],
                ["-n", "agent-fleet", "get", "deploy", "fleet-operator"],
                ["-n", "agent-fleet", "get", "fleetsandboxes"],
            ):
                proc = subprocess.run(
                    [kubectl, *args], capture_output=True, text=True, timeout=30
                )
                lines.append("$ kubectl " + " ".join(args))
                lines.append((proc.stdout or proc.stderr or "").strip() or "(no output)")
                lines.append("")
        else:
            lines.append("kubectl not on PATH — film host Fleet header only")

        html_path = self.out_dir.resolve() / "k8s-sandbox.html"
        html_path.write_text(
            "<!doctype html><meta charset=utf-8><title>k8s sandbox</title>"
            "<pre style='font:14px/1.4 ui-monospace,monospace;padding:24px'>"
            + _escape("\n".join(lines))
            + "</pre>",
            encoding="utf-8",
        )
        self.page.goto(html_path.as_uri(), wait_until="domcontentloaded")
        pause(self.page, 800)
        self.snap("kubectl")
        self.scene_fleet()

    def scene_kind(self) -> None:
        tree = subprocess.check_output(
            [str(REPO / "scripts" / "tree-id.sh")],
            cwd=str(REPO),
            text=True,
        ).strip()
        host_health: dict = {}
        kind_health: dict | None = None
        try:
            with urlopen(f"{API}/health", timeout=3) as r:
                host_health = json.loads(r.read().decode())
        except Exception as e:
            raise RuntimeError(f"this-tree API {API}/health not reachable: {e}") from e
        try:
            with urlopen("http://127.0.0.1:8080/health", timeout=2) as r:
                kind_health = json.loads(r.read().decode())
        except Exception:
            kind_health = None
        host_sha = str(host_health.get("git_sha") or "")
        kind_sha = str((kind_health or {}).get("git_sha") or "")
        if host_sha != tree:
            self.bug(f"kind scene host API sha {host_sha} != tree-id {tree}")
        if kind_health is not None and kind_sha == host_sha:
            note = "Kind :8080 matches this tree"
        elif kind_health is not None:
            note = (
                f"Kind :8080 is a different bake ({kind_sha}); "
                f"this-tree API is {API} ({host_sha})"
            )
        else:
            note = "Kind :8080 not reachable; filmed this-tree API only"
        payload = {
            "tree_id": tree,
            "this_tree_api": API,
            "this_tree_health": host_health,
            "kind_8080_health": kind_health,
            "note": note,
        }
        html_path = self.out_dir.resolve() / "kind-health.html"
        html_path.write_text(
            "<!doctype html><meta charset=utf-8><title>kind/host health</title>"
            "<pre style='font:14px/1.4 ui-monospace,monospace;padding:24px'>"
            + _escape(json.dumps(payload, indent=2))
            + "</pre>",
            encoding="utf-8",
        )
        self.page.goto(html_path.as_uri(), wait_until="domcontentloaded")
        pause(self.page, 1200)
        self.snap("health")


def _escape(s: str) -> str:
    return (
        s.replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
    )


SCENES: dict[str, Callable[[Demo], None]] = {
    "landing": Demo.scene_landing,
    "auth": Demo.scene_auth,
    "shell": Demo.scene_shell,
    "workspace": Demo.scene_workspace,
    "api": Demo.scene_api,
    "chat": Demo.scene_chat,
    "fleet": Demo.scene_fleet,
    "sandboxes": Demo.scene_sandboxes,
    "cases": Demo.scene_cases,
    "inbox": Demo.scene_inbox,
    "agents": Demo.scene_agents,
    "integrations": Demo.scene_integrations,
    "admin": Demo.scene_admin,
    "settings": Demo.scene_settings,
    "runners": Demo.scene_runners,
    "workers": Demo.scene_workers,
    "cloud": Demo.scene_cloud,
    "partners": Demo.scene_partners,
    "sessions": Demo.scene_sessions,
    "dashboard": Demo.scene_dashboard,
    "lab": Demo.scene_lab,
    "kind": Demo.scene_kind,
    "k8s": Demo.scene_k8s,
    "unify": Demo.scene_unify,
}

CORE = [
    "landing",
    "auth",
    "shell",
    "workspace",
    "api",
    "chat",
    "fleet",
    "sandboxes",
    "cases",
    "inbox",
    "agents",
    "integrations",
    "settings",
    "admin",
    "runners",
    "cloud",
    "partners",
    "sessions",
    "dashboard",
]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "scenes",
        nargs="*",
        help="scene names. Default: HUMAN_TEST_SCOPE.md (not all CORE). "
        "'all' = core. See --list.",
    )
    parser.add_argument(
        "--from-scope",
        action="store_true",
        help="use scenes from HUMAN_TEST_SCOPE.md (default when none given)",
    )
    parser.add_argument("--list", action="store_true", help="print scenes and exit")
    parser.add_argument(
        "--out",
        default="",
        help="output directory (default: <repo>/.tmp/demos)",
    )
    args = parser.parse_args(argv)

    if args.list:
        print("core:", " ".join(CORE))
        print("extra: lab kind unify")
        for name in SCENES:
            print(f"  {name}")
        return 0

    names = list(args.scenes)
    use_scope = args.from_scope or not names
    if use_scope and not names:
        from surface_scope import SCOPE, parse_scenes_from_scope

        if not SCOPE.is_file():
            raise SystemExit(
                "HUMAN_TEST_SCOPE.md missing. Run: "
                "python3 tests/browser/surface_scope.py --write"
            )
        names = parse_scenes_from_scope(SCOPE.read_text(encoding="utf-8"))
        if not names:
            print("HUMAN_TEST_SCOPE.md has no product scenes — skip film.")
            return 0
        print("from-scope:", " ".join(names), flush=True)
    if names == ["all"] or "all" in names:
        extra = [n for n in names if n not in ("all", *CORE)]
        names = CORE + extra
    unknown = [n for n in names if n not in SCENES]
    if unknown:
        raise SystemExit(f"unknown scenes: {unknown}. Use --list.")

    require_stack()
    sync_playwright = require_playwright()

    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    out_dir = Path(args.out) if args.out else (REPO / ".tmp" / "demos")
    out_dir = out_dir.resolve()
    video_dir = out_dir / f"{stamp}-{'-'.join(names[:6])}"
    video_dir.mkdir(parents=True, exist_ok=True)

    with sync_playwright() as pw:
        try:
            browser = pw.chromium.launch(headless=True)
        except Exception as e:
            raise SystemExit(
                f"Chromium launch failed ({e}). Need OS libraries "
                "(nss, atk, gtk, gbm, asound, …) and a browser under "
                ".tmp/ms-playwright. Re-run install-full-suite-deps.sh "
                "with ENABLE_BROWSER_E2E=1 in repo-root .env."
            ) from e
        context = browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
            record_video_dir=str(video_dir),
            record_video_size={"width": 1400, "height": 900},
        )
        page = context.new_page()
        page.set_default_timeout(25_000)
        frames_dir = out_dir / f"{stamp}-frames"
        frames_dir.mkdir(parents=True, exist_ok=True)
        demo = Demo(page, video_dir, frames_dir)
        try:
            for name in names:
                demo.scene = name
                print(f"==> scene {name}", flush=True)
                try:
                    SCENES[name](demo)
                except Exception as e:
                    demo.bug(f"scene aborted: {type(e).__name__}: {e}")
                    print(f"SCENE_FAIL {name}: {e}", flush=True)
                pause(page, 400)
                try:
                    demo.snap("end")
                    frame = frames_dir / f"{name}.png"
                    page.screenshot(path=str(frame), full_page=True)
                    print(f"FRAME {frame}", flush=True)
                except Exception as e:
                    print(f"FRAME_FAIL {name}: {e}", flush=True)
        finally:
            context.close()
            browser.close()

    videos = sorted(video_dir.glob("*.webm"))
    if not videos:
        raise SystemExit(f"Playwright produced no .webm in {video_dir}")
    # One context → one file; rename to a stable name.
    final = out_dir / f"{stamp}-{'-'.join(names[:8])}.webm"
    videos[0].replace(final)
    # Drop empty leftover dir files.
    for leftover in video_dir.glob("*"):
        if leftover.suffix == ".webm":
            leftover.replace(out_dir / f"{stamp}-extra-{leftover.name}")
    try:
        next(video_dir.iterdir())
    except StopIteration:
        video_dir.rmdir()
    print(f"DEMO_VIDEO={final}")
    print(f"REVIEW_FRAMES={frames_dir}")
    print(
        "REVIEW: inspect every mid-action frame and this .webm for bugs. "
        "Heading presence is not success — check that each click did the thing. "
        "Fill HUMAN_TEST_SCOPE.md Film review. Attach the .webm to the MR. "
        "Do not git add the video."
    )
    if demo.findings:
        print("BUGS_FOUND:", len(demo.findings), flush=True)
        for line in demo.findings:
            print(f"  - {line}", flush=True)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
