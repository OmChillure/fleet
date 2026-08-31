#!/usr/bin/env python3
"""Agent-recorded product demos (Playwright .webm).

Humans do not screen-record. The coding agent runs this (usually via
./scripts/human-test.sh record) on the final tree that will be merged.
Default scenes come from HUMAN_TEST_SCOPE.md (surface_scope.py). Film
every surface that merge can break, not a random walkthrough. After the
clip, review each scene frame + the .webm for bugs, fill the Film review
section in HUMAN_TEST_SCOPE.md, and attach the .webm to the GitLab MR.

  python3 tests/browser/record_demo.py --list
  python3 tests/browser/record_demo.py              # core UI scenes
  python3 tests/browser/record_demo.py fleet chat
  python3 tests/browser/record_demo.py --out .tmp/demos/custom

Requires UI :3030 + API :8080. Fail-closed if the stack or Playwright is missing.
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
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable
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


def _listening(host: str, port: int) -> bool:
    try:
        with socket.create_connection((host, port), timeout=2):
            return True
    except OSError:
        return False


def require_stack() -> None:
    try:
        require_this_tree(REPO, API)
    except ForeignStackError as e:
        raise SystemExit(str(e)) from e
    if not _listening("127.0.0.1", 3030):
        raise SystemExit(
            "UI :3030 is down. Start with ./scripts/human-test.sh launch "
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
    def __init__(self, page, out_dir: Path):
        self.page = page
        self.out_dir = out_dir
        self.email = f"demo.{uuid.uuid4().hex[:10]}@example.test"
        self.workspace_url: str | None = None

    def signup_login(self) -> None:
        p = self.page
        p.goto(f"{UI}/signup", wait_until="networkidle")
        pause(p)
        p.locator("#display_name").fill("Demo Agent")
        p.locator("#email").fill(self.email)
        p.locator("#password").fill(DEMO_PASSWORD)
        pause(p)
        p.get_by_role("button", name="Sign up").click()
        p.wait_for_url("**/login", timeout=20_000)
        pause(p)
        p.locator("#email").fill(self.email)
        p.locator("#password").fill(DEMO_PASSWORD)
        p.get_by_role("button", name="Log in").click()
        p.wait_for_url("**/dashboard", timeout=25_000)
        p.wait_for_selector("h1")
        self.workspace_url = p.url.rsplit("/dashboard", 1)[0]
        pause(p, 1000)

    def ensure_session(self) -> str:
        if not self.workspace_url:
            self.signup_login()
        assert self.workspace_url
        return self.workspace_url

    def scene_landing(self) -> None:
        p = self.page
        p.goto(f"{UI}/", wait_until="networkidle")
        pause(p, 1200)

    def scene_auth(self) -> None:
        # Scope order may film other pages first. Reuse that session;
        # a second Sign up with the same email never reaches /login.
        if not self.workspace_url:
            self.signup_login()
        p = self.page
        p.goto(self.workspace_url + "/dashboard", wait_until="networkidle")
        pause(p, 800)
        p.reload(wait_until="networkidle")
        pause(p, 800)
        if "/login" in p.url:
            raise RuntimeError("session cookie did not survive reload")

    def scene_shell(self) -> None:
        base = self.ensure_session()
        p = self.page
        p.goto(f"{base}/dashboard", wait_until="networkidle")
        pause(p)
        for label in ("Fleet", "Sandboxes", "New chat"):
            p.get_by_role("link", name=label, exact=True).first.click()
            pause(p, 600)
        expand_agents_nav(p)
        p.get_by_role("link", name="All agents", exact=True).first.click()
        pause(p)
        expand_workspace_nav(p)
        p.get_by_role("link", name="Integrations", exact=True).first.click()
        pause(p, 800)
        p.locator("button[aria-haspopup='menu']").click()
        pause(p, 900)

    def scene_workspace(self) -> None:
        base = self.ensure_session()
        p = self.page
        p.goto(f"{base}/dashboard", wait_until="networkidle")
        pause(p)
        # Workspace switcher (name + ▼) then "+ Create workspace".
        p.locator("aside").get_by_text("▼").first.click()
        pause(p, 400)
        p.get_by_role("button", name="+ Create workspace").click()
        pause(p, 400)
        p.get_by_placeholder("Workspace name").fill(f"Demo ws {uuid.uuid4().hex[:6]}")
        pause(p)
        p.get_by_role("button", name="Create", exact=True).click()
        p.wait_for_url("**/dashboard", timeout=15_000)
        pause(p, 1000)

    def scene_api(self) -> None:
        p = self.page
        p.goto(f"{API}/health", wait_until="domcontentloaded")
        pause(p, 1500)
        body = p.inner_text("body")
        if "fleet-server" not in body and "ok" not in body:
            raise RuntimeError(f"unexpected /health body: {body[:300]}")
        if "chat" not in body:
            raise RuntimeError(f"/health missing chat in apis (old server): {body[:400]}")
        p.goto(f"{API}/", wait_until="domcontentloaded")
        pause(p, 1200)

    def scene_chat(self) -> None:
        base = self.ensure_session()
        p = self.page
        p.goto(f"{base}/chat", wait_until="networkidle")
        pause(p, 1200)
        body = p.inner_text("body")
        if "Chat backend not connected yet." in body:
            raise RuntimeError("stub chat UI — not this tree's fleet-ui")
        if "Pick the runner by hand" not in body and "Select runner" not in body:
            raise RuntimeError("chat composer missing runner picker")
        # Do not fill a disabled textarea (false-green on stub; fails on new UI offline).
        box = p.locator("textarea")
        if box.count() and box.first.is_enabled():
            box.first.fill("Human-test ping (no send if provider is offline).")
            pause(p, 800)

    def scene_fleet(self) -> None:
        base = self.ensure_session()
        p = self.page
        p.goto(f"{base}/fleet", wait_until="networkidle")
        pause(p, 1000)
        start = p.get_by_role("button", name="Start 2-task run")
        if start.count() == 0:
            raise RuntimeError("Fleet page missing 'Start 2-task run'")
        start.first.click()
        pause(p, 1200)
        tick = p.get_by_role("button", name="Run tick")
        if tick.count():
            tick.first.click()
            pause(p, 1500)
            tick.first.click()
            pause(p, 1500)

    def scene_sandboxes(self) -> None:
        base = self.ensure_session()
        p = self.page
        p.goto(f"{base}/sandboxes", wait_until="networkidle")
        pause(p, 1500)

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
        title.first.fill("Demo case")
        policy.first.fill("Wait until observed, then stay open until cancelled.")
        create = p.get_by_role("button", name="Create case")
        if create.count() == 0:
            raise RuntimeError("Cases page missing 'Create case'")
        create.first.click()
        p.wait_for_url(re.compile(r"/cases/[0-9a-f-]{36}", re.I), timeout=15_000)
        pause(p, 1000)
        note = p.locator("input[placeholder*='Observe']")
        if note.count() == 0:
            raise RuntimeError("Case detail missing Observe field")
        note.first.fill("Demo observed a reply in Slack.")
        observe = p.get_by_role("button", name="Observe", exact=True)
        if observe.count() == 0:
            raise RuntimeError("Case detail missing Observe")
        observe.first.click()
        pause(p, 1500)
        cancel = p.get_by_role("button", name="Cancel", exact=True)
        if cancel.count() == 0:
            raise RuntimeError("Case detail missing Cancel")
        cancel.first.click()
        pause(p, 1500)

    def scene_inbox(self) -> None:
        from inbox_e2e_helpers import seed_connected_github, seed_open_ticket

        base = self.ensure_session()
        p = self.page
        p.goto(f"{base}/inbox", wait_until="networkidle")
        pause(p, 800)
        if p.locator("h1").first.inner_text().strip() != "Inbox":
            raise RuntimeError("Inbox page missing Inbox heading")
        for name in ("Start monitor", "Stop monitor", "Refresh now"):
            if p.get_by_role("button", name=name).count() == 0:
                raise RuntimeError(f"Inbox page missing {name}")
        ws = base.rstrip("/").rsplit("/", 1)[-1]
        seed_connected_github(ws)
        key = f"demo-inbox-{uuid.uuid4().hex[:8]}/board#1"
        title = "Demo inbox ticket"
        seed_open_ticket(ws, "github", key, title)
        start = p.get_by_role("button", name="Start monitor")
        start.first.click()
        pause(p, 1500)
        refresh = p.get_by_role("button", name="Refresh now")
        refresh.first.click()
        pause(p, 1200)
        p.get_by_text(f"{key} · {title}", exact=False).first.wait_for(timeout=15_000)
        finish = p.get_by_role("button", name="Finish", exact=True)
        if finish.count() == 0:
            raise RuntimeError("Inbox row missing Finish")
        finish.first.click()
        pause(p, 2000)

    def scene_agents(self) -> None:
        base = self.ensure_session()
        p = self.page
        p.goto(f"{base}/agents", wait_until="networkidle")
        pause(p, 800)
        seed = p.get_by_role("button", name="Seed starters")
        if seed.count():
            seed.first.click()
            pause(p, 1500)

    def scene_integrations(self) -> None:
        base = self.ensure_session()
        p = self.page
        p.goto(f"{base}/integrations", wait_until="networkidle")
        pause(p, 1000)
        search = p.locator("input[type='search'], input[placeholder*='Search' i]")
        if search.count():
            search.first.fill("git")
            pause(p, 1000)
            search.first.fill("trello")
            pause(p, 800)
            search.first.fill("asana")
            pause(p, 1200)

    def scene_settings(self) -> None:
        base = self.ensure_session()
        p = self.page
        p.goto(f"{base}/settings", wait_until="networkidle")
        pause(p, 800)
        if p.locator("h1").first.inner_text().strip() != "Settings":
            raise RuntimeError("Settings page missing Settings heading")
        heading = p.get_by_role("heading", name="Members", exact=True)
        if heading.count() == 0:
            raise RuntimeError("Settings page missing Members heading")
        heading.first.scroll_into_view_if_needed()
        pause(p, 600)
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
        invite_btn.first.click()
        p.get_by_text(
            "Invite sent — they join on signup or login.", exact=True
        ).wait_for(timeout=15_000)
        p.get_by_text(f"{guest} · member (pending)", exact=False).first.wait_for(
            timeout=15_000
        )
        pause(p, 1200)

    def scene_unify(self) -> None:
        """Isolation + runners + fleet tick — the unify-runners user path."""
        base = self.ensure_session()
        p = self.page
        expand_workspace_nav(p)
        p.goto(f"{base}/settings", wait_until="networkidle")
        pause(p, 700)
        iso = p.locator("select").filter(has=p.locator('option[value="container"]'))
        if iso.count():
            iso.first.select_option("container")
            pause(p, 400)
            save = p.get_by_role("button", name="Save")
            if save.count():
                save.first.click()
                pause(p, 800)
        expand_runners_nav(p)
        self._film_self_hosted("/runners/self-hosted")
        p.goto(f"{base}/fleet", wait_until="networkidle")
        pause(p, 600)
        start = p.get_by_role("button", name="Start 2-task run")
        if start.count():
            start.first.click()
            pause(p, 800)
        tick = p.get_by_role("button", name="Run tick")
        if tick.count():
            tick.first.click()
            pause(p, 1200)
        p.goto(f"{base}/sandboxes", wait_until="networkidle")
        pause(p, 800)

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
        if "Control plane" not in body and p.locator("button").filter(has_text="online").count() == 0:
            raise RuntimeError("self-hosted page missing control plane or cards")
        add = p.get_by_role("button", name="Add worker")
        if add.count():
            add.first.click()
            pause(p, 1500)
            unix_copy = (
                p.locator("pre")
                .filter(has_text="install-worker.sh")
                .locator("xpath=..")
                .get_by_role("button", name="Copy")
            )
            if unix_copy.count() and unix_copy.first.is_visible():
                unix_copy.first.click()
                pause(p, 600)
            cancel = p.get_by_role("button", name="Cancel")
            if cancel.count() and cancel.first.is_visible():
                pause(p, 400)
            done = p.get_by_role("button", name="Done")
            if done.count() and done.first.is_visible():
                done.first.click()
                pause(p, 600)
        card = p.locator("button").filter(has_text="last seen").or_(
            p.locator("button").filter(has_text="Control plane")
        )
        if card.count() and card.first.is_visible():
            card.first.click()
            pause(p, 800)
        popular = p.get_by_text("Popular models")
        if popular.count() and popular.first.is_visible():
            filt = p.get_by_placeholder("Filter the current top 50")
            if filt.count() and filt.first.is_visible():
                filt.first.fill("llama")
                pause(p, 600)
                filt.first.fill("")
                pause(p, 400)
        unlisted = p.get_by_placeholder("name or URL (llama3.2, hf.co/…)")
        if unlisted.count() and unlisted.first.is_visible():
            unlisted.first.fill("tinyllama")
            pause(p, 800)
            unlisted.first.fill("")
        models_dir = p.get_by_placeholder("~/.ollama/models")
        if models_dir.count() and models_dir.first.is_visible():
            pause(p, 400)
        sandbox = p.get_by_text("Sandboxing")
        if sandbox.count() and sandbox.first.is_visible():
            pause(p, 600)
        hold = p.get_by_role("button", name="Stop taking new jobs")
        if hold.count() and hold.first.is_visible():
            hold.first.click()
            pause(p, 1500)

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
        pause(p, 1500)

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
            ack.first.check()
            pause(p, 1200)

    def scene_sessions(self) -> None:
        base = self.ensure_session()
        p = self.page
        p.goto(f"{base}/sessions", wait_until="networkidle")
        pause(p, 1500)

    def scene_dashboard(self) -> None:
        base = self.ensure_session()
        p = self.page
        p.goto(f"{base}/dashboard", wait_until="networkidle")
        p.get_by_text("Dashboard", exact=True).first.wait_for(timeout=15_000)
        p.get_by_text("Open runs").first.wait_for(timeout=10_000)
        pause(p, 1500)

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
        pause(self.page, 2000)

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
        pause(self.page, 1500)
        self.scene_fleet()

    def scene_kind(self) -> None:
        try:
            with urlopen("http://127.0.0.1:8080/health", timeout=3) as r:
                health = json.loads(r.read().decode())
        except Exception as e:
            raise RuntimeError(f"kind/host API /health not reachable: {e}") from e
        html_path = self.out_dir.resolve() / "kind-health.html"
        html_path.write_text(
            "<!doctype html><meta charset=utf-8><title>kind/host health</title>"
            "<pre style='font:14px/1.4 ui-monospace,monospace;padding:24px'>"
            + _escape(json.dumps(health, indent=2))
            + "</pre>",
            encoding="utf-8",
        )
        self.page.goto(html_path.as_uri(), wait_until="domcontentloaded")
        pause(self.page, 2000)


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
        demo = Demo(page, video_dir)
        frames_dir = out_dir / f"{stamp}-frames"
        frames_dir.mkdir(parents=True, exist_ok=True)
        try:
            for name in names:
                print(f"==> scene {name}", flush=True)
                SCENES[name](demo)
                pause(page, 500)
                frame = frames_dir / f"{name}.png"
                page.screenshot(path=str(frame), full_page=True)
                print(f"FRAME {frame}", flush=True)
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
        "REVIEW: inspect every frame and this .webm for bugs. "
        "Fill HUMAN_TEST_SCOPE.md Film review. Attach the .webm to the MR. "
        "Do not git add the video."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
