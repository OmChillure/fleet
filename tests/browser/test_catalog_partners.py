#!/usr/bin/env python3
"""Layer 2 e2e: partners disclaimer ack persists (catalog:e2e-partners-ack-persists).

Ack is a recorded timestamp only. It must survive reload and must not
enable lending, buying credits, or a live credit balance.
"""
from __future__ import annotations

import json
import os
import re
import socket
import sys
import time
import unittest
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runners_e2e_helpers import UI, signup_login  # noqa: E402
from stack_identity import ForeignStackError, require_this_tree  # noqa: E402

API = os.environ.get("FLEET_API_URL", "http://127.0.0.1:8080").rstrip("/")
REPO = Path(__file__).resolve().parents[2]
BROWSERS = os.environ.get(
    "PLAYWRIGHT_BROWSERS_PATH",
    str(REPO / ".tmp" / "ms-playwright"),
)
os.environ.setdefault("PLAYWRIGHT_BROWSERS_PATH", BROWSERS)

_LEND_CONTROL_RE = re.compile(
    r"^(lend|lend capacity|enable lending|start lending|offer capacity|begin lending)$",
    re.I,
)
_ACK_LABEL = re.compile(r"acknowledge", re.I)
_ISO_TS = re.compile(
    r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$"
)


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


def _json(res) -> dict[str, Any]:
    text = res.text()
    if not res.ok:
        raise RuntimeError(f"{res.status} {res.url}: {text[:500]}")
    if not text:
        return {}
    return json.loads(text)


def _disclaimer(page, workspace_id: str) -> dict[str, Any]:
    return _json(
        page.request.get(f"{UI}/api/workspaces/{workspace_id}/partners/disclaimer")
    )


def _ack_boxes(page):
    boxes = page.get_by_role(
        "checkbox", name=re.compile(r"acknowledge|disclaimer", re.I)
    )
    if boxes.count() == 0:
        boxes = page.get_by_role("checkbox")
    return [boxes.nth(i) for i in range(boxes.count()) if boxes.nth(i).is_visible()]


def _goto_partners(page, base: str) -> None:
    """Land on Partners. dx rebuild overlay can hide h1; retry like signup."""
    last_err: Exception | None = None
    for _ in range(24):
        try:
            page.goto(
                f"{base}/runners/partners",
                wait_until="domcontentloaded",
                timeout=15_000,
            )
            page.locator("h1").wait_for(state="visible", timeout=8_000)
            if not re.search(r"/[0-9a-f-]{36}/runners/partners", page.url):
                raise RuntimeError(f"not on partners: {page.url}")
            last_err = None
            break
        except Exception as e:  # noqa: BLE001 — dx mid-rebuild; retry
            last_err = e
            time.sleep(2)
    if last_err is not None:
        raise last_err


def _assert_no_lend_enablement(test: unittest.TestCase, page, where: str) -> None:
    body = page.inner_text("body").lower()
    test.assertNotIn("lending enabled", body, f"{where}: lend must stay off")
    for i in range(page.get_by_role("button").count()):
        btn = page.get_by_role("button").nth(i)
        if not btn.is_visible():
            continue
        name = " ".join((btn.inner_text() or "").split())
        if not name or _LEND_CONTROL_RE.search(name) is None:
            continue
        test.assertFalse(
            btn.is_enabled(),
            f"{where}: lend control {name!r} must not be enabled",
        )


class CatalogPartnersAckE2ETests(unittest.TestCase):
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

    def test_ack_persists_and_does_not_enable_lend(self) -> None:
        page = self.page
        ws, base = signup_login(page, prefix="ack")

        before = _disclaimer(page, ws)
        self.assertFalse(before.get("accepted"), f"default must be off: {before}")
        self.assertTrue(
            before.get("accepted_at") in (None, ""),
            f"default accepted_at must be empty: {before}",
        )
        self.assertNotIn("lending", before)
        self.assertNotIn("assignable", before)

        _goto_partners(page, base)
        self.assertRegex(page.url, r"/[0-9a-f-]{36}/runners/partners(?:[?#].*)?$")

        visible = _ack_boxes(page)
        self.assertGreater(
            len(visible),
            0,
            "partners disclaimer control missing (expected a checkbox)",
        )
        box = visible[0]
        self.assertFalse(box.is_checked(), "disclaimer must default off")

        posted: dict[str, Any] = {}

        def is_ack_post(resp) -> bool:
            if "/partners/disclaimer" not in resp.url:
                return False
            if resp.request.method.upper() != "POST" or not resp.ok:
                return False
            try:
                posted.update(resp.json())
            except Exception:  # noqa: BLE001 — body may be empty mid-stream
                pass
            return True

        # Label click: locator.check() retries after the box disables itself.
        label = page.locator("label").filter(has_text=_ACK_LABEL).first
        label.wait_for()
        with page.expect_response(is_ack_post, timeout=15_000):
            label.click()
        self.assertTrue(box.is_checked(), "disclaimer must stay checked after persist")
        self.assertTrue(posted.get("accepted"), f"POST must record ack: {posted}")
        accepted_at = posted.get("accepted_at")
        self.assertIsInstance(accepted_at, str, f"ack is a timestamp only: {posted}")
        self.assertRegex(
            accepted_at,
            _ISO_TS,
            f"accepted_at must be an ISO timestamp: {posted}",
        )
        self.assertNotIn("lending", posted)
        self.assertNotIn("assignable", posted)

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
        visible = _ack_boxes(page)
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

        after = _disclaimer(page, ws)
        self.assertTrue(after.get("accepted"), f"GET after reload: {after}")
        self.assertEqual(
            after.get("accepted_at"),
            accepted_at,
            "ack timestamp must persist (first write wins)",
        )

        lend = page.get_by_role("button", name="Lend capacity")
        self.assertTrue(lend.first.is_visible(), "partners page missing Lend capacity")
        self.assertFalse(
            lend.first.is_enabled(),
            "Lend capacity must stay disabled after ack",
        )
        buy = page.get_by_role("button", name="Buy credits")
        self.assertTrue(buy.first.is_visible(), "partners page missing Buy credits")
        self.assertFalse(
            buy.first.is_enabled(),
            "Buy credits must stay disabled after ack",
        )
        picker = page.locator("select")
        self.assertGreater(picker.count(), 0, "partner picker missing")
        self.assertFalse(
            picker.first.is_enabled(),
            "partner picker must stay disabled after ack",
        )
        _assert_no_lend_enablement(self, page, "partners after ack")
        body_l = page.inner_text("body").lower()
        self.assertNotIn("balance", body_l, "ack must not create a credit balance")
        self.assertNotRegex(
            body_l,
            r"\b\d+(?:\.\d+)?\s+credits?\b",
            "ack must not display a live credit balance",
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
