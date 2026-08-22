#!/usr/bin/env python3
"""Layer 2 e2e: Copy Unix / Windows / token buttons (catalog:e2e-copy-buttons).

Copy buttons work, including the HTTP LAN execCommand fallback
(`copy_text` tries a hidden textarea first — navigator.clipboard is
HTTPS/localhost only).
"""
from __future__ import annotations

import json
import os
import socket
import sys
import time
import unittest
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runners_e2e_helpers import UI, goto_self_hosted, wait_dx_ready  # noqa: E402
from stack_identity import ForeignStackError, require_this_tree  # noqa: E402

API = os.environ.get("FLEET_API_URL", "http://127.0.0.1:8080").rstrip("/")
REPO = Path(__file__).resolve().parents[2]
BROWSERS = os.environ.get(
    "PLAYWRIGHT_BROWSERS_PATH",
    str(REPO / ".tmp" / "ms-playwright"),
)
os.environ.setdefault("PLAYWRIGHT_BROWSERS_PATH", BROWSERS)

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

_STUB_CLIPBOARD_API = """() => {
  const nav = window.navigator;
  if (window.__origClipboard === undefined) {
    window.__origClipboard = nav.clipboard;
  }
  try {
    Object.defineProperty(nav, "clipboard", {
      configurable: true,
      get() {
        return undefined;
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


def _poll_clipboard(
    page, needle: str, timeout_ms: int = 5_000, *, exclude: str | None = None
) -> str:
    """Read clipboard with a JS timeout so a stuck Clipboard API cannot hang."""
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


def _copy_button(page, pre_needle: str, *, last: bool = False):
    loc = page.locator("pre").filter(has_text=pre_needle)
    pre = loc.last if last else loc.first
    return pre.locator("xpath=..").get_by_role("button", name="Copy")


def _api_signup_workspace(page) -> tuple[str, str]:
    """Cookie-auth via the UI origin so we do not depend on signup WASM."""
    email = f"catalog-copy-{uuid.uuid4().hex[:12]}@example.test"
    headers = {"Content-Type": "application/json"}
    created = None
    last = ""
    for _ in range(12):
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
        if created.status in {200, 201}:
            break
        last = f"signup {created.status}: {created.text()[:400]}"
        time.sleep(2)
    else:
        raise AssertionError(last or "signup never succeeded")
    logged = page.request.post(
        f"{UI}/api/auth/login",
        data=json.dumps({"email": email, "password": "e2e-pass-ok"}),
        headers=headers,
    )
    if not logged.ok:
        raise AssertionError(f"login {logged.status}: {logged.text()[:400]}")
    body = logged.json()
    ws = ((body.get("default_workspace") or {}).get("id")) or ""
    if not ws:
        me = page.request.get(f"{UI}/api/auth/me")
        spaces = (me.json() or {}).get("workspaces") or []
        ws = str((spaces[0].get("id") if spaces else "") or "")
    if not ws:
        raise AssertionError(f"login missing workspace: {body!r}"[:400])
    return ws, f"{UI}/{ws}"


def _open_pair_panel(page, workspace_url: str) -> None:
    add = page.get_by_role("button", name="Add worker")
    deadline = time.time() + 180
    last_body = ""
    last_err: Exception | None = None
    # Re-goto when the first load is chrome-error / dx splash. A single
    # navigation sits on chrome-error:// if :3030 blips after signup.
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


def _last_exec_copy(page) -> str:
    copies = page.evaluate("() => window.__execCopies || []")
    if not copies:
        raise AssertionError("HTTP LAN fallback never called document.execCommand('copy')")
    last = copies[-1]
    if not isinstance(last, str):
        raise AssertionError(f"execCommand copy payload is not text: {last!r}")
    return last


class CatalogCopyButtonsTests(unittest.TestCase):
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

    def test_copy_unix_windows_token_incl_http_lan_fallback(self) -> None:
        # Isolated context so clipboard grants do not replace other catalog pages.
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
            _ws, workspace_url = _api_signup_workspace(page)
            _open_pair_panel(page, workspace_url)

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

            unix_btn = _copy_button(page, "install-worker.sh")
            win_btn = _copy_button(page, "install-worker.ps1")
            token_btn = _copy_button(page, "flp_", last=True)
            self.assertTrue(unix_btn.is_visible(), "Unix Copy missing")
            self.assertTrue(win_btn.is_visible(), "Windows Copy missing")
            self.assertTrue(token_btn.is_visible(), "token Copy missing")

            page.evaluate(_INSTALL_EXEC_SPY)
            self.assertTrue(
                page.evaluate(_STUB_CLIPBOARD_API),
                "must hide navigator.clipboard to exercise HTTP LAN fallback",
            )

            unix_btn.click(force=True)
            page.get_by_role("status").filter(has_text="Command copied.").first.wait_for(
                timeout=5_000
            )
            unix_fb = _last_exec_copy(page)
            self.assertIn("install-worker.sh", unix_fb)
            self.assertIn("FLEET_PAIR_TOKEN=", unix_fb)
            self.assertIn(token, unix_fb)

            win_btn.click(force=True)
            page.get_by_role("status").filter(has_text="Command copied.").first.wait_for(
                timeout=5_000
            )
            win_fb = _last_exec_copy(page)
            self.assertIn("install-worker.ps1", win_fb)
            self.assertIn("FLEET_PAIR_TOKEN=", win_fb)
            self.assertIn(token, win_fb)
            self.assertNotIn(
                "install-worker.sh",
                win_fb,
                "Windows Copy must not leave the Unix command on the fallback path",
            )

            token_btn.click(force=True)
            page.get_by_role("status").filter(has_text="Token copied.").first.wait_for(
                timeout=5_000
            )
            tok_fb = _last_exec_copy(page)
            self.assertIn(token, tok_fb)
            self.assertNotIn(
                "install-worker",
                tok_fb,
                "token Copy must not paste an install command",
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

            unix_btn.click(force=True)
            unix_clip = _poll_clipboard(page, "install-worker.sh")
            self.assertIn("FLEET_PAIR_TOKEN=", unix_clip)
            self.assertIn(token, unix_clip)

            win_btn.click(force=True)
            win_clip = _poll_clipboard(page, "install-worker.ps1")
            self.assertIn("FLEET_PAIR_TOKEN=", win_clip)
            self.assertIn(token, win_clip)
        finally:
            ctx.close()


if __name__ == "__main__":
    unittest.main(verbosity=2)
