#!/usr/bin/env python3
"""Layer 2 e2e: Auto-update off + Update now (catalog:e2e-update-now).

Button shows when the box is behind and auto-update is off; click becomes
Update requested.
"""
from __future__ import annotations

import json
import os
import socket
import sys
import unittest
import uuid
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runners_e2e_helpers import (  # noqa: E402
    UI,
    goto_self_hosted,
    heartbeat,
    list_hosts,
    pair_and_heartbeat,
    patch_host,
    wait_dx_ready,
)
from stack_identity import ForeignStackError, require_this_tree  # noqa: E402

API = os.environ.get("FLEET_API_URL", "http://127.0.0.1:8080").rstrip("/")
REPO = Path(__file__).resolve().parents[2]
BROWSERS = os.environ.get(
    "PLAYWRIGHT_BROWSERS_PATH",
    str(REPO / ".tmp" / "ms-playwright"),
)
os.environ.setdefault("PLAYWRIGHT_BROWSERS_PATH", BROWSERS)

TOAST = "Update requested. The worker applies it on the next heartbeat if it can."
BEHIND_SHA = "0000000000000000000000000000000000000000"
BEHIND_BINS = {
    "fleet-host": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
    "fleet-driver": "0123456701234567012345670123456701234567012345670123456701234567",
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


def _needs_software_update(host: dict[str, Any]) -> bool:
    status = host.get("software_status")
    if host.get("outdated") or status == "behind":
        return True
    if status == "current":
        sw = host.get("software") or {}
        sha = (sw.get("git_sha") or "").strip() if isinstance(sw, dict) else ""
        return not sha
    return False


def _staged_linux(listed: dict[str, Any]) -> tuple[str, dict[str, str]]:
    sha = ""
    bins: dict[str, str] = {}
    for pkg in listed.get("packages") or []:
        if not isinstance(pkg, dict):
            continue
        if pkg.get("platform") != "linux-x86_64":
            continue
        sha = (pkg.get("git_sha") or "").strip()
        raw = pkg.get("bins") or {}
        if isinstance(raw, dict):
            for stem, val in raw.items():
                if isinstance(val, dict):
                    got = (val.get("sha256") or "").strip()
                else:
                    got = str(val).strip()
                if got:
                    bins[stem] = got
        break
    return sha, bins


def _mismatch_software(listed: dict[str, Any] | None) -> dict[str, Any]:
    staged_sha, staged_bins = _staged_linux(listed or {})
    git_sha = BEHIND_SHA
    if staged_sha and git_sha.lower() == staged_sha.lower():
        git_sha = "1111111111111111111111111111111111111111"
    bins = dict(BEHIND_BINS)
    for stem, want in staged_bins.items():
        if stem in bins and bins[stem].lower() == want.lower():
            bins[stem] = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
    return {"git_sha": git_sha, "bins": bins}


def _host_from_hb(hb: dict[str, Any]) -> dict[str, Any]:
    host = hb.get("host")
    return host if isinstance(host, dict) else {}


def _signup_api(page) -> tuple[str, str]:
    """Cookie session via API so a stuck dx rebuild banner cannot hang signup."""
    email = f"catalog-updnow-{uuid.uuid4().hex[:12]}@example.test"
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
                    "display_name": "E2E catalog-updnow",
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
    ws = str(((body.get("default_workspace") or {}).get("id")) or "")
    if not ws:
        me = page.request.get(f"{UI}/api/auth/me")
        spaces = (me.json() or {}).get("workspaces") or []
        ws = str((spaces[0].get("id") if spaces else "") or "")
    if not ws:
        raise RuntimeError(f"login missing workspace: {body!r}"[:400])
    return ws, f"{UI}/{ws}"


def _wait_control_plane(page, workspace_url: str) -> None:
    last_err: Exception | None = None
    for _ in range(8):
        goto_self_hosted(page, workspace_url)
        wait_dx_ready(page, timeout_ms=30_000)
        try:
            page.get_by_text("Control plane", exact=True).first.wait_for(
                state="attached", timeout=15_000
            )
            return
        except Exception as e:  # noqa: BLE001 — dx mid-rebuild / list still loading
            last_err = e
            page.wait_for_timeout(1_500)
    body = ""
    try:
        body = page.inner_text("body")[:1500]
    except Exception:
        pass
    raise AssertionError(
        f"Control plane missing after pair. url={page.url} body={body!r}"
    ) from last_err


class CatalogUpdateNowTests(unittest.TestCase):
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
        cls.page.set_default_timeout(30_000)

    @classmethod
    def tearDownClass(cls) -> None:
        cls._context.close()
        cls._browser.close()
        cls._pw.stop()

    def test_update_now_when_behind_becomes_update_requested(self) -> None:
        page = self.page
        ws, workspace_url = _signup_api(page)
        suffix = uuid.uuid4().hex[:12]
        name = f"updnow-box-{suffix}"

        listed = list_hosts(page, ws)
        software = _mismatch_software(listed)
        created, paired, hb = pair_and_heartbeat(
            page,
            ws,
            name,
            heartbeat_extra={"software": software},
        )
        host = _host_from_hb(hb)
        machine = paired["machine_token"]
        if not _needs_software_update(host):
            # Old package: bins only, no bake id.
            hb = heartbeat(
                page,
                machine,
                {"software": {"bins": {"fleet-host": "not-the-staged-sha"}}},
            )
            host = _host_from_hb(hb)
        if not _needs_software_update(host):
            listed = list_hosts(page, ws)
            hb = heartbeat(
                page,
                machine,
                {"software": _mismatch_software(listed)},
            )
            host = _host_from_hb(hb)
        self.assertTrue(
            _needs_software_update(host),
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

        _wait_control_plane(page, workspace_url)

        plane = page.locator("article").filter(has_text="Control plane").first
        self.assertEqual(
            plane.get_by_role("button", name="Update now").count(),
            0,
            "Control plane must not show Update now",
        )

        card = page.locator("article").filter(has_text=name)
        try:
            card.first.wait_for(state="attached", timeout=30_000)
        except Exception:
            self.fail(
                f"paired card {name!r} missing. "
                f"url={page.url} body={page.inner_text('body')[:1500]!r}"
            )
        update_now = card.get_by_role("button", name="Update now")
        if update_now.count() == 0 or not update_now.first.is_visible():
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

        toast = page.get_by_role("status").filter(has_text=TOAST)
        try:
            toast.first.wait_for(timeout=5_000)
        except Exception:
            toast = page.get_by_role("status").filter(has_text="Update requested")
            try:
                toast.first.wait_for(timeout=1_000)
            except Exception:
                toast = None
        if toast is not None and toast.count() > 0:
            self.assertTrue(toast.first.is_visible(), "toast present but hidden")
            self.assertIn("Update requested", toast.first.inner_text())


if __name__ == "__main__":
    unittest.main()
