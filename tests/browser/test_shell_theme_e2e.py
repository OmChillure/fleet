#!/usr/bin/env python3
"""Shell theme + Fleet button contrast (catalog:list-ui-shell-theme-e2e).

Playwright drives the user menu and Fleet chrome. Locks:
  - Appearance toggles Black/White
  - Escape closes the user menu
  - Start 2-task run stays readable on Black (not white-on-white)

  python3 -m pytest tests/browser/test_shell_theme_e2e.py -q
  python3 -m unittest tests.browser.test_shell_theme_e2e -v
"""
from __future__ import annotations

import os
import socket
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runners_e2e_helpers import signup_login, wait_dx_ready  # noqa: E402
from stack_identity import ForeignStackError, require_this_tree  # noqa: E402

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
    if not _listening("127.0.0.1", 3030) and not _listening("127.0.0.1", 13230):
        # Host remap writes FLEET_UI_URL; default listen is :3030.
        from urllib.parse import urlparse

        from runners_e2e_helpers import UI

        parsed = urlparse(UI)
        port = parsed.port or 3030
        host = "127.0.0.1" if parsed.hostname in (None, "localhost") else parsed.hostname
        if not _listening(host, port):
            raise RuntimeError(
                "UI is not listening. Start this tree: ./scripts/human-test.sh launch"
            )


def _require_playwright():
    try:
        from playwright.sync_api import sync_playwright
    except ImportError as e:
        raise RuntimeError(
            "playwright is not installed. Run "
            "crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh "
            "with ENABLE_BROWSER_E2E=1."
        ) from e
    return sync_playwright


def _contrast(page, locator) -> dict:
    return locator.first.evaluate(
        """el => {
            const s = getComputedStyle(el);
            const parse = (c) => {
                const m = String(c).match(/rgba?\\((\\d+),\\s*(\\d+),\\s*(\\d+)/);
                return m ? [+m[1], +m[2], +m[3]] : null;
            };
            const lum = (rgb) => 0.2126*rgb[0] + 0.7152*rgb[1] + 0.0722*rgb[2];
            const fg = parse(s.color);
            const bg = parse(s.backgroundColor);
            return {
                text: (el.innerText || '').trim(),
                color: s.color,
                bg: s.backgroundColor,
                contrast: (fg && bg) ? Math.abs(lum(fg) - lum(bg)) : null,
            };
        }"""
    )


class ShellThemeE2ETests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        _require_stack()
        sync_playwright = _require_playwright()
        cls._pw = sync_playwright().start()
        try:
            cls._browser = cls._pw.chromium.launch(headless=True)
        except Exception as e:
            cls._pw.stop()
            raise RuntimeError(f"Chromium launch failed ({e})") from e

    @classmethod
    def tearDownClass(cls) -> None:
        cls._browser.close()
        cls._pw.stop()

    def test_appearance_toggle_and_escape_closes_overlay(self) -> None:
        from playwright.sync_api import expect
        from runners_e2e_helpers import UI

        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            _ws, base = signup_login(page, prefix="theme-e2e")
            wait_dx_ready(page)
            page.goto(f"{base}/dashboard", wait_until="domcontentloaded")
            expect(page.get_by_role("heading").first).to_be_visible()

            page.locator("button[aria-haspopup='menu']").click()
            appearance = page.get_by_role("button", name="Appearance")
            expect(appearance).to_contain_text("Black")
            appearance.click()
            expect(appearance).to_contain_text("White")
            expect(appearance).not_to_contain_text("Black")

            page.keyboard.press("Escape")
            expect(appearance).to_have_count(0)
        finally:
            ctx.close()

    def test_fleet_start_button_readable_in_dark(self) -> None:
        from playwright.sync_api import expect
        from runners_e2e_helpers import UI

        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        ctx.add_init_script("localStorage.setItem('fleet-theme', 'black')")
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            _ws, base = signup_login(page, prefix="fleet-contrast")
            wait_dx_ready(page)
            page.goto(f"{base}/fleet", wait_until="domcontentloaded")
            start = page.get_by_role("button", name="Start 2-task run")
            expect(start.first).to_be_visible()
            # Root class applies --app-accent / --app-accent-fg; no role for it.
            expect(page.locator(".theme-black").first).to_be_attached()

            info = _contrast(page, start)
            self.assertEqual(info.get("text"), "Start 2-task run", info)
            contrast = info.get("contrast")
            self.assertIsNotNone(contrast, info)
            self.assertGreater(
                contrast,
                50,
                f"Start 2-task run unreadable in dark: {info}",
            )
        finally:
            ctx.close()


if __name__ == "__main__":
    unittest.main(verbosity=2)
