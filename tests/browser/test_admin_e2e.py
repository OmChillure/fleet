#!/usr/bin/env python3
"""Instance admin page e2e.

/{admin}: overview, users, promote/demote. Non-admin sees the gate.
Fail-closed: this-tree identity + Playwright.
"""
from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import unittest
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runners_e2e_helpers import UI, wait_dx_ready  # noqa: E402
from stack_identity import ForeignStackError, require_this_tree  # noqa: E402

API = os.environ.get("FLEET_API_URL", "http://127.0.0.1:8080").rstrip("/")
REPO = Path(__file__).resolve().parents[2]
BROWSERS = os.environ.get(
    "PLAYWRIGHT_BROWSERS_PATH",
    str(REPO / ".tmp" / "ms-playwright"),
)
os.environ.setdefault("PLAYWRIGHT_BROWSERS_PATH", BROWSERS)

_PASSWORD = "e2e-pass-ok"


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
    if not _listening("127.0.0.1", 3030) and not _listening("127.0.0.1", 13030):
        host = "127.0.0.1"
        # Human-test host ports when Kind owns :3030.
        pass
    parsed_host = "127.0.0.1"
    from urllib.parse import urlparse

    port = urlparse(UI).port or 3030
    if not _listening(parsed_host, port):
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


def database_url() -> str:
    url = os.environ.get("DATABASE_URL", "").strip()
    if url:
        return url
    env = REPO / ".env"
    if env.is_file():
        for line in env.read_text(encoding="utf-8").splitlines():
            if line.startswith("DATABASE_URL="):
                return line.split("=", 1)[1].strip().strip('"').strip("'")
    raise RuntimeError("DATABASE_URL is required to promote an instance admin")


def promote_email(email: str) -> None:
    sql = (
        "UPDATE users SET instance_admin = TRUE, updated_at = now() "
        f"WHERE email_normalized = lower(trim('{email}')) RETURNING email;"
    )
    out = subprocess.check_output(
        ["psql", database_url(), "-v", "ON_ERROR_STOP=1", "-tAc", sql],
        text=True,
    )
    if email.lower() not in out.lower():
        raise RuntimeError(f"promote did not update {email}: {out!r}")


def _signup_login(page, prefix: str) -> str:
    email = f"{prefix}-{uuid.uuid4().hex[:12]}@example.test"
    page.goto(f"{UI}/signup", wait_until="domcontentloaded", timeout=20_000)
    page.locator("#display_name").fill(f"E2E {prefix}", force=True)
    page.locator("#email").fill(email, force=True)
    page.locator("#password").fill(_PASSWORD, force=True)
    page.get_by_role("button", name="Sign up").click(force=True)
    page.wait_for_url("**/login", timeout=20_000)
    page.locator("#email").fill(email, force=True)
    page.locator("#password").fill(_PASSWORD, force=True)
    page.get_by_role("button", name="Log in").click(force=True)
    page.wait_for_url("**/dashboard", timeout=25_000)
    return email


class AdminE2E(unittest.TestCase):
    def test_non_admin_is_gated_and_admin_can_promote(self) -> None:
        _require_stack()
        pw = _require_playwright()
        with pw() as p:
            browser = p.chromium.launch(headless=True)
            page = browser.new_page()
            wait_dx_ready(page)
            email = _signup_login(page, "admin-e2e")
            page.goto(f"{UI}/admin", wait_until="networkidle")
            page.get_by_text("Instance admin required", exact=False).wait_for(
                timeout=10_000
            )
            aside = page.locator("aside").inner_text()
            self.assertNotIn("Instance", aside.split("Workspace")[-1] if "Workspace" in aside else aside)

            promote_email(email)
            page.reload(wait_until="networkidle")
            page.get_by_role("heading", name="Instance", exact=True).wait_for(
                timeout=15_000
            )
            page.get_by_role("heading", name="Users", exact=True).wait_for()
            self.assertGreater(page.get_by_text(email, exact=False).count(), 0)
            guest = f"admin-guest-{uuid.uuid4().hex[:8]}@example.test"
            res = page.request.post(
                f"{UI}/api/auth/signup",
                data=json.dumps(
                    {
                        "email": guest,
                        "password": _PASSWORD,
                        "display_name": "Guest",
                    }
                ),
                headers={"content-type": "application/json"},
            )
            self.assertEqual(res.status, 201, res.text())
            page.reload(wait_until="networkidle")
            row = page.locator("tr").filter(has_text=guest)
            row.first.wait_for(timeout=15_000)
            row.get_by_role("button", name="Promote", exact=True).click()
            page.get_by_text("Promoted to instance admin.", exact=True).wait_for(
                timeout=10_000
            )
            row = page.locator("tr").filter(has_text=guest)
            row.get_by_role("button", name="Demote", exact=True).click()
            page.get_by_text("Demoted instance admin.", exact=True).wait_for(
                timeout=10_000
            )
            browser.close()


if __name__ == "__main__":
    unittest.main()
