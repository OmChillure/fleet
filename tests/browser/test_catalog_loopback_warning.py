#!/usr/bin/env python3
"""Layer 2 e2e: loopback URL warning (catalog:e2e-loopback-warning).

Pair panel warns when the install command is loopback
(`hint_is_loopback`: 127.0.0.1 / localhost / [::1]). API_PUBLIC_BASE may
be a LAN address; rewrite the create-host hint so this layer-2 copy is
still exercised. A public hint must not show the warning.
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
from runners_e2e_helpers import UI, click_add_worker, wait_dx_ready  # noqa: E402
from stack_identity import ForeignStackError, require_this_tree  # noqa: E402

API = os.environ.get("FLEET_API_URL", "http://127.0.0.1:8080").rstrip("/")
REPO = Path(__file__).resolve().parents[2]
BROWSERS = os.environ.get(
    "PLAYWRIGHT_BROWSERS_PATH",
    str(REPO / ".tmp" / "ms-playwright"),
)
os.environ.setdefault("PLAYWRIGHT_BROWSERS_PATH", BROWSERS)

HOST_RE = re.compile(r"/api/workspaces/[^/]+/worker-hosts/?$")
LOOPBACK_MARKERS = ("127.0.0.1", "localhost", "[::1]")
LOOPBACK_URL = "http://127.0.0.1:8080"
PUBLIC_URL = "https://fleet.example.test"
WARNING_SNIPPET = "loopback URL"
LAN_PUBLIC_RE = re.compile(r"LAN or public")


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


def _signup_api(page) -> tuple[str, str]:
    """Cookie session via API so a stuck dx rebuild banner cannot hang signup."""
    email = f"catalog-loopback-{uuid.uuid4().hex[:12]}@example.test"
    password = "e2e-pass-ok"
    headers = {"Content-Type": "application/json"}
    created = None
    last = ""
    for _ in range(12):
        created = page.request.post(
            f"{UI}/api/auth/signup",
            data=json.dumps(
                {
                    "email": email,
                    "password": password,
                    "display_name": "E2E catalog-loopback",
                }
            ),
            headers=headers,
        )
        if created.status in {200, 201}:
            break
        last = f"signup {created.status}: {created.text()[:400]}"
        time.sleep(2)
    else:
        raise AssertionError(last or "signup never succeeded")
    logged = page.request.post(
        f"{UI}/api/auth/login",
        data=json.dumps({"email": email, "password": password}),
        headers=headers,
    )
    if not logged.ok:
        raise AssertionError(f"login {logged.status}: {logged.text()[:400]}")
    body = logged.json()
    ws = str(((body.get("default_workspace") or {}).get("id")) or "")
    if not ws:
        me = page.request.get(f"{UI}/api/auth/me")
        spaces = (me.json() or {}).get("workspaces") or []
        ws = str((spaces[0].get("id") if spaces else "") or "")
    if not ws:
        raise AssertionError(f"login missing workspace: {body!r}"[:400])
    return ws, f"{UI}/{ws}"


def _rewrite_install_hint(data: dict, replacement: str) -> dict:
    hint = data.get("install_hint") or ""
    token = data.get("pair_token") or "flp_missing"
    if not hint:
        data["install_hint"] = (
            f"curl -fsSL {replacement}/install-worker.sh | "
            f"FLEET_URL={replacement} FLEET_PAIR_TOKEN={token} sh"
        )
        return data
    data["install_hint"] = re.sub(r"https?://[^/\s]+", replacement, hint)
    return data


def _hint_rewriter(replacement: str):
    def _force(route):
        if route.request.method.upper() != "POST":
            route.continue_()
            return
        try:
            response = route.fetch()
            data = response.json()
        except Exception:
            route.continue_()
            return
        data = _rewrite_install_hint(data if isinstance(data, dict) else {}, replacement)
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

    return _force


def _open_pair_panel(page, workspace_url: str, replacement: str) -> str:
    add = page.get_by_role("button", name="Add worker")
    heading = page.get_by_role("heading", name="Pair this worker")
    rewriter = _hint_rewriter(replacement)
    last_body = ""
    last_err: Exception | None = None
    # One navigation. Re-goto resets WASM. Overlay may cover a live Add worker.
    page.goto(
        f"{workspace_url}/runners/self-hosted",
        wait_until="domcontentloaded",
    )
    page.route(HOST_RE, rewriter)
    deadline = time.time() + 180
    try:
        while time.time() < deadline:
            if not _listening("127.0.0.1", 3030) or not _listening("127.0.0.1", 8080):
                raise AssertionError(
                    "stack died while waiting for pair panel "
                    f"(ui={_listening('127.0.0.1', 3030)} "
                    f"api={_listening('127.0.0.1', 8080)})"
                )
            try:
                wait_dx_ready(page, timeout_ms=2_000)
                last_body = page.inner_text("body")
                # Overlay can sit on a live WASM app (other dx rebuilds).
                # force-click like the other catalog pair-panel tests.
                if heading.count() and heading.first.is_visible():
                    panel = heading.first.locator("xpath=ancestor::section[1]")
                    return panel.inner_text()
                if add.count() and add.first.is_visible():
                    click_add_worker(page)
                    heading.wait_for(state="visible", timeout=20_000)
                    panel = heading.locator("xpath=ancestor::section[1]")
                    return panel.inner_text()
            except Exception as e:
                last_err = e
            page.wait_for_timeout(500)
        raise AssertionError(
            "Add worker / Pair this worker missing (dx rebuild overlay?). "
            f"url={page.url} body={last_body[:1500]!r} err={last_err}"
        )
    finally:
        page.unroute(HOST_RE, rewriter)


class CatalogLoopbackWarningTests(unittest.TestCase):
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

    def test_warning_when_command_is_loopback(self) -> None:
        # catalog:e2e-loopback-warning — Warning when command is loopback.
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        page.set_default_navigation_timeout(30_000)
        try:
            _ws, workspace_url = _signup_api(page)
            text = _open_pair_panel(page, workspace_url, LOOPBACK_URL)
            self.assertIn(
                WARNING_SNIPPET,
                text,
                f"pair panel must warn on loopback command: {text[:800]!r}",
            )
            self.assertRegex(
                text,
                LAN_PUBLIC_RE,
                f"warning must mention LAN or public URL: {text[:800]!r}",
            )
            self.assertTrue(
                any(m in text.lower() for m in LOOPBACK_MARKERS),
                f"rewritten command must still be loopback: {text[:800]!r}",
            )
        finally:
            ctx.close()

    def test_no_warning_when_command_is_public(self) -> None:
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        page.set_default_navigation_timeout(30_000)
        try:
            _ws, workspace_url = _signup_api(page)
            text = _open_pair_panel(page, workspace_url, PUBLIC_URL)
            self.assertNotIn(
                WARNING_SNIPPET,
                text,
                f"pair panel must not warn on a public command: {text[:800]!r}",
            )
            self.assertIn(
                PUBLIC_URL,
                text,
                f"rewritten public hint missing from panel: {text[:800]!r}",
            )
        finally:
            ctx.close()


if __name__ == "__main__":
    unittest.main(verbosity=2)
