#!/usr/bin/env python3
"""Usability drive for unify-runners: a simulated user walks the menus
while the backend is poked to produce each behavior this branch touched.

Real inputs: type signup/login, click sidebar, Add worker, Isolation
select + Save, Start 2-task run, Run tick, open Sandboxes / Cloud /
Partners. Backend at the same time pairs hosts, heartbeats software, and
sets isolation so the user can see assign / skip / behind / placeholders.

Artifacts (gitignored): `.tmp/usability/<stamp>/`
  report.json, steps.md, screenshots, console.log, log-excerpt.txt

Fail-closed: this-tree identity + Playwright + :3030/:8080.
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
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runners_e2e_helpers import (  # noqa: E402
    create_host,
    goto_self_hosted,
    heartbeat,
    list_hosts,
    pair_host,
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
            "playwright is not installed. Run the one install script "
            "with ENABLE_BROWSER_E2E=1."
        ) from e
    return sync_playwright


def _json(res) -> Any:
    text = res.text()
    if not res.ok:
        raise RuntimeError(f"{res.status} {res.url}: {text[:500]}")
    if not text:
        return {}
    return json.loads(text)


class Journal:
    def __init__(self, out: Path) -> None:
        self.out = out
        self.shots = out / "screenshots"
        self.shots.mkdir(parents=True, exist_ok=True)
        self.steps: list[dict[str, Any]] = []
        self.console: list[str] = []
        self.n = 0

    def shot(self, page, name: str) -> str:
        safe = re.sub(r"[^a-z0-9_-]+", "-", name.lower()).strip("-")
        path = self.shots / f"{self.n:02d}-{safe}.png"
        page.screenshot(path=str(path), full_page=True)
        return str(path.relative_to(self.out))

    def step(
        self,
        page,
        title: str,
        *,
        user: str,
        backend: str = "",
        expect: str = "",
        extra: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        self.n += 1
        wait_dx_ready(page)
        body = ""
        try:
            body = page.inner_text("body")
        except Exception as e:  # noqa: BLE001
            body = f"<body unreadable: {e}>"
        rec = {
            "n": self.n,
            "title": title,
            "user": user,
            "backend": backend,
            "url": page.url,
            "expect": expect,
            "heading": "",
            "body_excerpt": body[:1200],
            "screenshot": self.shot(page, title),
        }
        if page.locator("h1").count():
            rec["heading"] = page.locator("h1").first.inner_text()
        if extra:
            rec["extra"] = extra
        self.steps.append(rec)
        return rec

    def write(self, analysis: dict[str, Any]) -> None:
        (self.out / "report.json").write_text(
            json.dumps(
                {"steps": self.steps, "analysis": analysis, "console": self.console},
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )
        lines = ["# Unify usability drive\n"]
        for s in self.steps:
            lines.append(f"## {s['n']}. {s['title']}\n")
            lines.append(f"- user: {s['user']}")
            if s.get("backend"):
                lines.append(f"- backend: {s['backend']}")
            lines.append(f"- url: {s['url']}")
            if s.get("heading"):
                lines.append(f"- heading: {s['heading']}")
            if s.get("expect"):
                lines.append(f"- expect: {s['expect']}")
            if s.get("extra"):
                lines.append(f"- extra: `{json.dumps(s['extra'])[:500]}`")
            lines.append(f"- shot: {s['screenshot']}\n")
        lines.append("## Analysis\n")
        lines.append("```json")
        lines.append(json.dumps(analysis, indent=2))
        lines.append("```\n")
        (self.out / "steps.md").write_text("\n".join(lines), encoding="utf-8")
        (self.out / "console.log").write_text(
            "\n".join(self.console) + "\n", encoding="utf-8"
        )


def _expand(page, label: str) -> None:
    loc = page.locator("aside nav").get_by_text(label, exact=True).first
    if loc.count():
        loc.click()
        page.wait_for_timeout(300)


def _click_nav(page, name: str) -> None:
    nav = page.locator("aside nav")
    link = nav.get_by_role("link", name=name, exact=True)
    if link.count() == 0 or not link.first.is_visible():
        # parent accordion
        for parent in ("Runners", "Monitoring", "Workspace", "Agents", "Chat"):
            _expand(page, parent)
            link = nav.get_by_role("link", name=name, exact=True)
            if link.count() and link.first.is_visible():
                break
    link.first.click()


def _pair_named(
    page,
    ws: str,
    name: str,
    *,
    advertised_host: bool,
    advertised_container: bool,
    software: dict[str, Any] | None = None,
) -> dict[str, Any]:
    created = create_host(page, ws, name)
    paired = pair_host(
        page,
        created["pair_token"],
        hostname=name,
        advertised_host=advertised_host,
        advertised_container=advertised_container,
    )
    extra: dict[str, Any] = {
        "advertised_host": advertised_host,
        "advertised_container": advertised_container,
        "hostname": name,
        "platform": "linux-x86_64",
        "max_concurrent": 4,
    }
    if software:
        extra["software"] = software
    hb = heartbeat(page, paired["machine_token"], extra)
    return {"created": created, "paired": paired, "hb": hb}


def _excerpt_logs(out: Path) -> None:
    log_dir = REPO / ".run" / "logs"
    chunks: list[str] = []
    for name in ("fleet-server.log", "fleet-host.log", "fleet-ui.log"):
        p = log_dir / name
        if not p.is_file():
            chunks.append(f"===== {name} MISSING =====\n")
            continue
        text = p.read_text(encoding="utf-8", errors="replace")
        lines = text.splitlines()
        tail = lines[-200:] if len(lines) > 200 else lines
        interesting = [
            ln
            for ln in tail
            if re.search(
                r"tick|spawn|assign|isolation|loopback|claim|heartbeat|"
                r"FLEET_HARNESS|result\.md|behind|min_isolation|refuse",
                ln,
                re.I,
            )
        ]
        chunks.append(f"===== {name} tail =====\n")
        chunks.append("\n".join(tail[-80:]))
        chunks.append(f"\n===== {name} matches =====\n")
        chunks.append("\n".join(interesting[-80:]))
        chunks.append("\n")
    (out / "log-excerpt.txt").write_text("\n".join(chunks), encoding="utf-8")


def _analyze(page, ws: str, journal: Journal) -> dict[str, Any]:
    hosts = list_hosts(page, ws)
    settings = _json(page.request.get(f"{UI}/api/workspaces/{ws}/fleet/settings"))
    tasks = _json(page.request.get(f"{UI}/api/workspaces/{ws}/fleet/tasks"))
    sessions = _json(page.request.get(f"{UI}/api/workspaces/{ws}/fleet/sessions"))
    health = _json(page.request.get(f"{API}/health"))
    host_rows = hosts.get("hosts") or hosts.get("worker_hosts") or []
    if isinstance(hosts, list):
        host_rows = hosts
    task_rows = tasks.get("tasks") or []
    sess_rows = sessions.get("sessions") or []
    findings: list[str] = []
    ok = True

    iso = settings.get("min_isolation")
    if iso != "container":
        ok = False
        findings.append(f"settings min_isolation is {iso!r}, expected container")
    else:
        findings.append("settings min_isolation=container (user saved it)")

    names = {h.get("name") or h.get("hostname") for h in host_rows if isinstance(h, dict)}
    if "studio-box" not in names:
        findings.append(f"studio-box not in host list: {names}")
    if "lab-box" not in names:
        findings.append(f"lab-box not in host list: {names}")

    statuses = {t.get("plan_task_id"): t.get("status") for t in task_rows}
    remote = [s for s in sess_rows if s.get("runtime_backend") == "remote"]
    stub = [s for s in sess_rows if s.get("runtime_backend") == "stub"]
    if stub:
        ok = False
        findings.append(f"local stub session appeared: {stub}")
    else:
        findings.append("no stub session (plane is not a runner)")

    if remote:
        findings.append(
            f"{len(remote)} remote session(s) after container host appeared: "
            f"{[s.get('runtime_handle') for s in remote]}"
        )
    else:
        # With container min_isolation, a process-only host must not assign.
        # After lab-box (container) is paired, a later tick may enqueue.
        ready = [t for t in task_rows if t.get("status") == "ready"]
        assigned = [t for t in task_rows if t.get("status") in ("assigned", "in_progress")]
        findings.append(
            f"no remote session yet; ready={len(ready)} assigned={len(assigned)} "
            f"statuses={statuses}"
        )

    behind = [
        h.get("name")
        for h in host_rows
        if isinstance(h, dict)
        and (
            h.get("outdated")
            or h.get("software_status") == "behind"
        )
    ]
    if behind:
        findings.append(f"software behind/outdated on {behind}")
    else:
        findings.append("no host reported behind (studio-box software poke may lack a staged manifest)")

    return {
        "ok": ok,
        "git_sha": health.get("git_sha"),
        "min_isolation": iso,
        "host_names": sorted(n for n in names if n),
        "task_statuses": statuses,
        "remote_sessions": len(remote),
        "stub_sessions": len(stub),
        "behind_hosts": behind,
        "findings": findings,
        "step_count": len(journal.steps),
        "console_errors": [c for c in journal.console if "error" in c.lower()],
    }


def run_drive(out: Path) -> dict[str, Any]:
    _require_stack()
    sync_playwright = _require_playwright()
    journal = Journal(out)

    with sync_playwright() as pw:
        browser = pw.chromium.launch(headless=True)
        context = browser.new_context(
            viewport={"width": 1440, "height": 900},
            locale="en-US",
        )
        page = context.new_page()
        page.on(
            "console",
            lambda msg: journal.console.append(f"{msg.type}: {msg.text}"),
        )

        # 1. Landing
        page.goto(f"{UI}/", wait_until="domcontentloaded", timeout=20_000)
        wait_dx_ready(page)
        started = page.get_by_role("link", name=re.compile(r"Get started|Sign up", re.I))
        if started.count():
            started.first.click()
        journal.step(
            page,
            "landing",
            user="Maya opens the product and clicks Get started",
            expect="signup form",
        )

        # 2–3. Signup + login via helper (same fields a human types)
        ws, base = signup_login(page, prefix="maya.unify")
        journal.step(
            page,
            "dashboard",
            user="Maya signs up as maya.unify-*@example.test and logs in",
            expect="workspace dashboard",
            extra={"workspace_id": ws},
        )

        # 4. Sidebar → Runners → Self-hosted
        _expand(page, "Runners")
        _click_nav(page, "Self-hosted")
        page.wait_for_url("**/runners/self-hosted", timeout=15_000)
        wait_dx_ready(page)
        add = page.get_by_role("button", name="Add worker")
        if add.count():
            add.first.click()
            page.wait_for_timeout(800)
        journal.step(
            page,
            "self-hosted-add-worker",
            user="Maya expands Runners, opens Self-hosted, clicks Add worker",
            expect="install command / pair token panel",
        )
        done = page.get_by_role("button", name="Done")
        if done.count() and done.first.is_visible():
            done.first.click()

        # 5. Backend: process-only host
        studio = _pair_named(
            page,
            ws,
            "studio-box",
            advertised_host=True,
            advertised_container=False,
        )
        goto_self_hosted(page, base)
        page.wait_for_timeout(500)
        journal.step(
            page,
            "studio-box-online",
            user="Maya stays on Self-hosted while her laptop phones home",
            backend="pair+heartbeat studio-box advertised_host=true advertised_container=false",
            expect="studio-box card online",
            extra={"host_id": studio["paired"].get("host_id")},
        )

        # 6. Settings isolation = container
        _expand(page, "Workspace")
        _click_nav(page, "Settings")
        page.wait_for_url("**/settings", timeout=15_000)
        wait_dx_ready(page)
        iso = page.locator("select").filter(has=page.locator('option[value="container"]'))
        if iso.count() == 0:
            iso = page.locator("select").nth(0)
        iso.first.select_option("container")
        page.get_by_role("button", name="Save").click()
        page.wait_for_timeout(800)
        settings = _json(page.request.get(f"{UI}/api/workspaces/{ws}/fleet/settings"))
        journal.step(
            page,
            "settings-isolation-container",
            user="Maya opens Settings, sets Isolation to Container, clicks Save",
            backend=f"GET settings → min_isolation={settings.get('min_isolation')}",
            expect="Isolation stays Container after save",
            extra={"settings": settings},
        )
        if settings.get("min_isolation") != "container":
            raise AssertionError(
                f"Save did not persist min_isolation=container: {settings}"
            )

        # 7. Fleet: start run + tick (process-only host must not take it)
        _expand(page, "Monitoring")
        _click_nav(page, "Fleet")
        page.wait_for_url("**/fleet", timeout=15_000)
        wait_dx_ready(page)
        page.get_by_role("button", name="Start 2-task run").click()
        page.wait_for_timeout(600)
        page.get_by_role("button", name="Run tick").click()
        page.wait_for_timeout(1200)
        tasks_after = _json(page.request.get(f"{UI}/api/workspaces/{ws}/fleet/tasks"))
        sess_after = _json(page.request.get(f"{UI}/api/workspaces/{ws}/fleet/sessions"))
        journal.step(
            page,
            "fleet-tick-skipped-process-only",
            user="Maya starts a 2-task run and clicks Run tick",
            backend="tick with min_isolation=container and process-only studio-box",
            expect="tasks stay ready; no stub session",
            extra={"tasks": tasks_after, "sessions": sess_after},
        )
        for s in sess_after.get("sessions") or []:
            if s.get("runtime_backend") == "stub":
                raise AssertionError(f"plane stub ran a job: {s}")

        # 8. Backend: container-capable host
        lab = _pair_named(
            page,
            ws,
            "lab-box",
            advertised_host=True,
            advertised_container=True,
        )
        goto_self_hosted(page, base)
        page.wait_for_timeout(500)
        journal.step(
            page,
            "lab-box-container",
            user="Maya checks Self-hosted after the lab machine pairs",
            backend="pair+heartbeat lab-box advertised_container=true",
            expect="two runner cards (studio-box + lab-box)",
            extra={"host_id": lab["paired"].get("host_id")},
        )

        # 9. Tick again — container host can take the job
        _click_nav(page, "Fleet")
        page.wait_for_url("**/fleet", timeout=15_000)
        wait_dx_ready(page)
        page.get_by_role("button", name="Run tick").click()
        page.wait_for_timeout(1500)
        sess2 = _json(page.request.get(f"{UI}/api/workspaces/{ws}/fleet/sessions"))
        journal.step(
            page,
            "fleet-tick-assigns-container",
            user="Maya clicks Run tick again after the lab box is online",
            backend="tick should enqueue remote on lab-box",
            expect="remote session or still-ready (never stub)",
            extra={"sessions": sess2},
        )

        # 10. Sandboxes
        _click_nav(page, "Sandboxes")
        page.wait_for_url("**/sandboxes", timeout=15_000)
        wait_dx_ready(page)
        journal.step(
            page,
            "sandboxes",
            user="Maya opens Sandboxes to see the same sessions",
            expect="session list or empty hint naming Fleet",
        )

        # 11. Software behind on studio-box
        heartbeat(
            page,
            studio["paired"]["machine_token"],
            {
                "advertised_host": True,
                "advertised_container": False,
                "software": {
                    "git_sha": "old-tree",
                    "bins": {"fleet-host": "aaa"},
                },
            },
        )
        goto_self_hosted(page, base)
        page.wait_for_timeout(800)
        listed = list_hosts(page, ws)
        journal.step(
            page,
            "software-behind",
            user="Maya looks at runner cards after studio-box reports no fleet-driver",
            backend="heartbeat software bins={fleet-host only}",
            expect="behind/outdated if a staged manifest exists",
            extra={"hosts": listed},
        )

        # 12. Cloud + Partners placeholders
        _click_nav(page, "Cloud")
        page.wait_for_url("**/runners/cloud", timeout=15_000)
        wait_dx_ready(page)
        journal.step(
            page,
            "cloud",
            user="Maya opens Runners → Cloud",
            expect="AWS/GCP/Azure + Fleet Cloud placeholders",
        )
        _click_nav(page, "Partners")
        page.wait_for_url("**/runners/partners", timeout=15_000)
        wait_dx_ready(page)
        journal.step(
            page,
            "partners",
            user="Maya opens Runners → Partners",
            expect="trust copy; lend/buy disabled",
        )

        # 13. /workers redirect
        page.goto(f"{base}/workers", wait_until="domcontentloaded")
        wait_dx_ready(page)
        page.wait_for_url("**/runners/self-hosted", timeout=15_000)
        journal.step(
            page,
            "workers-redirect",
            user="Maya opens the old /workers URL",
            expect="redirect to /runners/self-hosted",
        )

        analysis = _analyze(page, ws, journal)
        context.close()
        browser.close()

    _excerpt_logs(out)
    journal.write(analysis)
    return analysis


class UnifyUsabilityTests(unittest.TestCase):
    def test_menu_drive_with_backend_states(self):
        stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        out = REPO / ".tmp" / "usability" / stamp
        out.mkdir(parents=True, exist_ok=True)
        (REPO / ".tmp" / "usability" / "LATEST").write_text(str(out) + "\n")
        analysis = run_drive(out)
        print(json.dumps(analysis, indent=2), flush=True)
        print(f"artifacts: {out}", flush=True)
        self.assertTrue(
            analysis["ok"],
            f"usability findings: {analysis.get('findings')}",
        )
        self.assertGreaterEqual(analysis["step_count"], 8)
        self.assertEqual(analysis["min_isolation"], "container")
        self.assertEqual(analysis["stub_sessions"], 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
