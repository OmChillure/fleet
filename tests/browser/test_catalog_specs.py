#!/usr/bin/env python3
"""Layer 2 e2e: specs on a live self-hosted card (catalog:e2e-specs).

Seeds a paired host via pair_and_heartbeat with a hardware snapshot that
must survive specs_line / clean_cpu_model / fmt_mem / fmt_gpu, then asserts
CPU, cores, RAM, Intel iGPU, and load % on that card (not Control plane).
"""
from __future__ import annotations

import os
import socket
import sys
import unittest
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runners_e2e_helpers import (  # noqa: E402
    goto_self_hosted,
    list_hosts,
    pair_and_heartbeat,
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

# Must survive specs_line (see page.rs specs_line_shows_intel_uhd).
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


class CatalogSpecsE2ETests(unittest.TestCase):
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

    def test_specs_on_a_live_card(self) -> None:
        page = self.page
        _ws, workspace_url = signup_login(page, prefix="specs")
        name = f"specs-i7-{uuid.uuid4().hex[:8]}"
        created, paired, hb = pair_and_heartbeat(
            page,
            _ws,
            name,
            heartbeat_extra={"hardware": SPECS_HARDWARE},
        )
        self.assertIn("pair_token", created)
        self.assertIn("machine_token", paired)
        hw = (hb.get("host") or {}).get("hardware") or {}
        self.assertEqual(hw.get("cpu_model"), SPECS_HARDWARE["cpu_model"], hb)
        listed = list_hosts(page, _ws)
        row = next(
            (h for h in (listed.get("hosts") or []) if h.get("name") == name),
            None,
        )
        self.assertIsNotNone(row, listed)
        persisted = (row or {}).get("hardware") or {}
        self.assertEqual(persisted.get("cpu_cores"), 8, row)
        self.assertEqual(persisted.get("mem_type"), "DDR4", row)
        gpus = persisted.get("gpus") or []
        self.assertTrue(
            any((g or {}).get("name") == "Intel UHD Graphics" for g in gpus),
            row,
        )

        goto_self_hosted(page, workspace_url)
        # Card must come from the list, not an in-page poll.
        page.reload(wait_until="domcontentloaded")
        wait_dx_ready(page)
        page.wait_for_selector("h1", state="attached", timeout=60_000)

        plane = page.locator("article").first
        plane.wait_for(timeout=10_000)
        plane_text = plane.inner_text()
        self.assertIn("Control plane", plane_text)
        self.assertNotIn("i7-10610U", plane_text)
        self.assertNotIn("Intel UHD Graphics", plane_text)

        card = page.locator("article").filter(has_text=name)
        self.assertGreater(
            card.count(),
            0,
            f"host card {name!r} missing (not Control plane). "
            f"body={page.inner_text('body')[:800]!r}",
        )
        card.first.get_by_text("i7-10610U").wait_for(timeout=15_000)
        text = card.first.inner_text()
        self.assertNotIn("Control plane", text)

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


if __name__ == "__main__":
    unittest.main(verbosity=2)
