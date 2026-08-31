#!/usr/bin/env python3
"""Settings members/invites browser e2e (catalog:list-ui-settings-members-e2e).

Playwright drives the live Settings Members chrome — not a helper or HTTP-only
lock. HTTP invite/leave/409 cases live in fleet-auth; this file owns the page.

Intended product (human-test "Settings members"):
  /{ws}/settings — Members: invite by email (member/admin).
  Existing users join immediately; others join on signup or next login.
  Owner can change roles / remove; member cannot invite.

Fail-closed: this-tree identity (`/health.git_sha` + UI `/fleet-git-sha.txt`
== `scripts/tree-id.sh`) and Playwright against :3030 / :8080. Missing stack
or a foreign Kind image is a fail, not a skip.

  python3 -m pytest tests/browser/test_settings_members_e2e.py -q
  python3 -m unittest tests.browser.test_settings_members_e2e -v
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
from runners_e2e_helpers import UI, signup_login, wait_dx_ready  # noqa: E402
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


def _goto_settings(page, workspace_url: str) -> None:
    last_err: Exception | None = None
    for _ in range(6):
        try:
            page.goto(f"{workspace_url}/settings", wait_until="domcontentloaded")
        except Exception as e:  # noqa: BLE001 — dx restart / connection reset
            last_err = e
            time.sleep(2)
            continue
        wait_dx_ready(page)
        try:
            page.wait_for_selector("h1", state="attached", timeout=20_000)
            page.get_by_role("heading", name="Members", exact=True).wait_for(
                state="visible", timeout=20_000
            )
            return
        except Exception as e:  # noqa: BLE001 — dx mid-rebuild; retry
            last_err = e
            time.sleep(2)
    raise RuntimeError(f"could not open settings members: {last_err}")


def _json(res) -> dict:
    text = res.text()
    if not res.ok:
        raise RuntimeError(f"{res.status} {res.url}: {text[:500]}")
    if not text:
        return {}
    return json.loads(text)


def _me(page) -> dict:
    return _json(page.request.get(f"{UI}/api/auth/me"))


def _members(page, workspace_id: str) -> dict:
    return _json(page.request.get(f"{UI}/api/workspaces/{workspace_id}/members"))


def _invites(page, workspace_id: str) -> dict:
    return _json(page.request.get(f"{UI}/api/workspaces/{workspace_id}/invites"))


def _workspaces(page) -> list[dict]:
    body = _json(page.request.get(f"{UI}/api/workspaces"))
    rows = body.get("workspaces") or []
    return [w for w in rows if isinstance(w, dict)]


def _norm(email: str) -> str:
    return email.strip().lower()


def _find_member(body: dict, email: str) -> dict | None:
    want = _norm(email)
    for m in body.get("members") or []:
        if isinstance(m, dict) and _norm(str(m.get("email") or "")) == want:
            return m
    return None


def _find_invite(body: dict, email: str) -> dict | None:
    want = _norm(email)
    for inv in body.get("invites") or []:
        if isinstance(inv, dict) and _norm(str(inv.get("email") or "")) == want:
            return inv
    return None


def _wait_member(
    page, workspace_id: str, email: str, *, role: str | None = None, timeout_s: float = 20.0
) -> dict:
    deadline = time.time() + timeout_s
    last: dict = {}
    last_err: Exception | None = None
    while time.time() < deadline:
        try:
            last = _members(page, workspace_id)
            last_err = None
            hit = _find_member(last, email)
            if hit is not None and (role is None or hit.get("role") == role):
                return hit
        except Exception as e:  # noqa: BLE001 — poll until roster is readable
            last_err = e
        page.wait_for_timeout(400)
    raise AssertionError(
        f"member {email!r} role={role!r} missing: last={last!r} err={last_err}"[:800]
    )


def _wait_no_member(
    page, workspace_id: str, email: str, timeout_s: float = 20.0
) -> dict:
    deadline = time.time() + timeout_s
    last: dict = {}
    while time.time() < deadline:
        last = _members(page, workspace_id)
        if _find_member(last, email) is None:
            return last
        page.wait_for_timeout(400)
    raise AssertionError(f"member {email!r} still listed: {last!r}"[:800])


def _wait_invite(
    page, workspace_id: str, email: str, *, role: str | None = None, timeout_s: float = 20.0
) -> dict:
    deadline = time.time() + timeout_s
    last: dict = {}
    last_err: Exception | None = None
    while time.time() < deadline:
        try:
            last = _invites(page, workspace_id)
            last_err = None
            hit = _find_invite(last, email)
            if hit is not None and (role is None or hit.get("role") == role):
                return hit
        except Exception as e:  # noqa: BLE001 — poll until invites are readable
            last_err = e
        page.wait_for_timeout(400)
    raise AssertionError(
        f"invite {email!r} role={role!r} missing: last={last!r} err={last_err}"[:800]
    )


def _wait_no_invite(
    page, workspace_id: str, email: str, timeout_s: float = 20.0
) -> dict:
    deadline = time.time() + timeout_s
    last: dict = {}
    while time.time() < deadline:
        last = _invites(page, workspace_id)
        if _find_invite(last, email) is None:
            return last
        page.wait_for_timeout(400)
    raise AssertionError(f"invite {email!r} still open: {last!r}"[:800])


def _invite_form(page):
    email_box = page.locator("input[type='email'][placeholder='teammate@example.test']")
    email_box.first.wait_for(state="visible", timeout=15_000)
    form = email_box.first.locator("xpath=..")
    role_select = form.locator("select")
    invite_btn = form.get_by_role("button", name="Invite", exact=True)
    return email_box.first, role_select.first, invite_btn.first


def _member_row(page, email: str):
    return page.locator("li").filter(has_text=email).filter(
        has_not_text=re.compile(r"\(pending\)")
    )


def _pending_row(page, email: str, role: str):
    return page.get_by_text(f"{email} · {role} (pending)", exact=False)


def _signup_account(browser, *, prefix: str) -> tuple[str, str]:
    """Create a user in a throwaway context so the owner session stays put."""
    email = f"{prefix}-{uuid.uuid4().hex[:12]}@example.test"
    ctx = browser.new_context(
        base_url=UI,
        viewport={"width": 1400, "height": 900},
    )
    page = ctx.new_page()
    try:
        headers = {"Content-Type": "application/json"}
        created = None
        last_err = ""
        for _ in range(12):
            created = page.request.post(
                f"{UI}/api/auth/signup",
                data=json.dumps(
                    {
                        "email": email,
                        "password": _PASSWORD,
                        "display_name": f"E2E {prefix}",
                    }
                ),
                headers=headers,
            )
            if created.status in {200, 201}:
                return email, _PASSWORD
            last_err = f"signup {created.status}: {created.text()[:400]}"
            page.wait_for_timeout(750)
        raise RuntimeError(last_err or "signup failed")
    finally:
        ctx.close()


def _login_ui(page, email: str, password: str) -> tuple[str, str]:
    last_err: Exception | None = None
    for _ in range(24):
        try:
            page.goto(f"{UI}/login", wait_until="domcontentloaded", timeout=15_000)
            page.locator("#email").wait_for(state="attached", timeout=8_000)
            last_err = None
            break
        except Exception as e:  # noqa: BLE001 — dx mid-rebuild; retry
            last_err = e
            time.sleep(2)
    if last_err is not None:
        raise last_err
    wait_dx_ready(page)
    page.locator("#email").fill(email, force=True)
    page.locator("#password").fill(password, force=True)
    page.get_by_role("button", name="Log in").click(force=True)
    page.wait_for_url("**/dashboard", timeout=20_000)
    url = page.url.rsplit("/dashboard", 1)[0]
    ws = url.rstrip("/").rsplit("/", 1)[-1]
    return ws, url


def _signup_login_as(page, email: str, *, prefix: str) -> tuple[str, str]:
    """Drive the signup form so pending invites are accepted on that path."""
    last_err: Exception | None = None
    for _ in range(24):
        try:
            page.goto(f"{UI}/signup", wait_until="domcontentloaded", timeout=15_000)
            page.locator("#display_name").wait_for(state="attached", timeout=8_000)
            last_err = None
            break
        except Exception as e:  # noqa: BLE001 — dx mid-rebuild; retry
            last_err = e
            time.sleep(2)
    if last_err is not None:
        raise last_err
    wait_dx_ready(page)
    page.locator("#display_name").fill(f"E2E {prefix}", force=True)
    page.locator("#email").fill(email, force=True)
    page.locator("#password").fill(_PASSWORD, force=True)
    page.get_by_role("button", name="Sign up").click(force=True)
    page.wait_for_url("**/login", timeout=15_000)
    return _login_ui(page, email, _PASSWORD)


class SettingsMembersE2ETests(unittest.TestCase):
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

    def test_unauthenticated_settings_redirects_to_login(self) -> None:
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            fake = uuid.uuid4()
            page.goto(f"{UI}/{fake}/settings", wait_until="domcontentloaded")
            page.wait_for_url("**/login", timeout=15_000)
            self.assertIn("/login", page.url)
        finally:
            ctx.close()

    def test_owner_invite_existing_role_remove_and_pending_revoke(self) -> None:
        ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        page = ctx.new_page()
        page.set_default_timeout(20_000)
        try:
            ws, workspace_url = signup_login(page, prefix="set-mem-own")
            me = _me(page)
            owner_email = str((me.get("user") or {}).get("email") or "")
            self.assertTrue(owner_email, me)
            _goto_settings(page, workspace_url)
            self.assertEqual(page.locator("h1").first.inner_text().strip(), "Settings")
            self.assertIn("/settings", page.url)
            self.assertIn(ws, page.url)
            body = page.inner_text("body")
            self.assertIn("Invite by email", body)
            self.assertIn("Existing users join immediately", body)
            self.assertIn("signup or next login", body)
            self.assertIn("Roles: owner, admin, member", body)

            email_box, role_select, invite_btn = _invite_form(page)
            role_values = role_select.locator("option").evaluate_all(
                "els => els.map(e => e.value)"
            )
            self.assertEqual(
                role_values,
                ["member", "admin"],
                "owner invite roles must be member and admin (not owner)",
            )
            self.assertTrue(invite_btn.is_disabled(), "Invite must stay disabled until email")

            owner_row = _member_row(page, owner_email)
            owner_row.first.wait_for(state="visible", timeout=15_000)
            self.assertIn("owner", owner_row.first.inner_text())
            self.assertEqual(
                owner_row.get_by_role("button", name="Leave", exact=True).count(),
                0,
                "last owner must not see Leave",
            )
            self.assertEqual(
                owner_row.get_by_role("button", name="Remove", exact=True).count(),
                0,
                "last owner must not see Remove on self",
            )
            roster = _members(page, ws)
            self.assertEqual((roster.get("you") or {}).get("role"), "owner", roster)
            self.assertIsNotNone(_find_member(roster, owner_email), roster)

            existing_email, _pw = _signup_account(self._browser, prefix="set-mem-exist")
            email_box.fill(existing_email)
            role_select.select_option("member")
            self.assertTrue(invite_btn.is_enabled(), "Invite stayed disabled after email")
            invite_btn.click()
            page.get_by_text("Added existing user.", exact=True).wait_for(timeout=15_000)
            added = _wait_member(page, ws, existing_email, role="member")
            self.assertEqual(added.get("role"), "member", added)
            _member_row(page, existing_email).first.wait_for(timeout=15_000)
            self.assertIn(
                "member",
                _member_row(page, existing_email).first.inner_text(),
            )
            listed = _invites(page, ws)
            self.assertIsNone(
                _find_invite(listed, existing_email),
                f"existing user must join immediately, not stay pending: {listed}",
            )
            self.assertEqual(
                _pending_row(page, existing_email, "member").count(),
                0,
                "existing user must not appear as (pending)",
            )

            peer_row = _member_row(page, existing_email)
            peer_role = peer_row.locator("select")
            self.assertGreater(
                peer_role.count(),
                0,
                "owner must have a role select on another member",
            )
            peer_role.first.select_option("admin")
            page.get_by_text("Role updated.", exact=True).wait_for(timeout=15_000)
            promoted = _wait_member(page, ws, existing_email, role="admin")
            self.assertEqual(promoted.get("role"), "admin", promoted)
            page.get_by_text(re.compile(r"\badmin\b")).first.wait_for(timeout=15_000)
            self.assertIn("admin", peer_row.first.inner_text())

            remove = peer_row.get_by_role("button", name="Remove", exact=True)
            self.assertTrue(remove.count() > 0, "owner must see Remove on a non-owner")
            remove.first.click()
            page.get_by_text("Member removed.", exact=True).wait_for(timeout=15_000)
            _wait_no_member(page, ws, existing_email)
            hide_deadline = time.time() + 15
            while time.time() < hide_deadline:
                if _member_row(page, existing_email).count() == 0:
                    break
                page.wait_for_timeout(200)
            self.assertEqual(
                _member_row(page, existing_email).count(),
                0,
                "removed member must leave the Members list",
            )

            pending_email = f"set-mem-pend-{uuid.uuid4().hex[:12]}@example.test"
            email_box, role_select, invite_btn = _invite_form(page)
            email_box.fill(pending_email)
            role_select.select_option("admin")
            invite_btn.click()
            page.get_by_text(
                "Invite sent — they join on signup or login.", exact=True
            ).wait_for(timeout=15_000)
            pending = _wait_invite(page, ws, pending_email, role="admin")
            self.assertEqual(pending.get("role"), "admin", pending)
            self.assertTrue(pending.get("id"), pending)
            pend_ui = _pending_row(page, pending_email, "admin")
            pend_ui.first.wait_for(timeout=15_000)
            self.assertIsNone(
                _find_member(_members(page, ws), pending_email),
                "unknown email must stay pending until signup/login",
            )

            revoke = page.locator("li").filter(has_text=pending_email).get_by_role(
                "button", name="Revoke", exact=True
            )
            self.assertTrue(revoke.count() > 0, "pending invite missing Revoke")
            revoke.first.click()
            _wait_no_invite(page, ws, pending_email)
            hide_inv = time.time() + 15
            while time.time() < hide_inv:
                if _pending_row(page, pending_email, "admin").count() == 0:
                    break
                page.wait_for_timeout(200)
            self.assertEqual(
                _pending_row(page, pending_email, "admin").count(),
                0,
                "revoked invite must leave the pending list",
            )
        finally:
            ctx.close()

    def test_pending_invite_joins_on_signup(self) -> None:
        owner_ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        owner = owner_ctx.new_page()
        owner.set_default_timeout(20_000)
        guest_ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        guest = guest_ctx.new_page()
        guest.set_default_timeout(20_000)
        try:
            ws, workspace_url = signup_login(owner, prefix="set-mem-host")
            _goto_settings(owner, workspace_url)
            guest_email = f"set-mem-guest-{uuid.uuid4().hex[:12]}@example.test"
            email_box, role_select, invite_btn = _invite_form(owner)
            email_box.fill(guest_email)
            role_select.select_option("member")
            invite_btn.click()
            owner.get_by_text(
                "Invite sent — they join on signup or login.", exact=True
            ).wait_for(timeout=15_000)
            _wait_invite(owner, ws, guest_email, role="member")
            _pending_row(owner, guest_email, "member").first.wait_for(timeout=15_000)

            guest_ws, _guest_url = _signup_login_as(
                guest, guest_email, prefix="set-mem-guest"
            )
            self.assertNotEqual(
                guest_ws,
                ws,
                "signup still creates the guest's own default workspace",
            )
            ids = [str(w.get("id") or "") for w in _workspaces(guest)]
            self.assertIn(
                ws,
                ids,
                f"guest must join the invited workspace on signup: {ids}",
            )
            guest_members = _json(
                guest.request.get(f"{UI}/api/workspaces/{ws}/members")
            )
            self.assertEqual(
                (guest_members.get("you") or {}).get("role"),
                "member",
                guest_members,
            )
            self.assertIsNotNone(_find_member(guest_members, guest_email), guest_members)

            _goto_settings(guest, f"{UI}/{ws}")
            self.assertIn(ws, guest.url)
            self.assertIn("/settings", guest.url)
            _member_row(guest, guest_email).first.wait_for(timeout=15_000)

            joined = _wait_member(owner, ws, guest_email, role="member")
            self.assertEqual(joined.get("role"), "member", joined)
            _wait_no_invite(owner, ws, guest_email)
            owner.reload(wait_until="domcontentloaded")
            wait_dx_ready(owner)
            owner.get_by_role("heading", name="Members", exact=True).wait_for(
                timeout=20_000
            )
            _member_row(owner, guest_email).first.wait_for(timeout=15_000)
            self.assertEqual(
                _pending_row(owner, guest_email, "member").count(),
                0,
                "accepted invite must not stay (pending)",
            )
        finally:
            guest_ctx.close()
            owner_ctx.close()

    def test_member_cannot_invite(self) -> None:
        owner_ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        owner = owner_ctx.new_page()
        owner.set_default_timeout(20_000)
        member_ctx = self._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        member = member_ctx.new_page()
        member.set_default_timeout(20_000)
        try:
            ws, workspace_url = signup_login(owner, prefix="set-mem-lead")
            member_email, member_pw = _signup_account(
                self._browser, prefix="set-mem-peer"
            )
            _goto_settings(owner, workspace_url)
            email_box, role_select, invite_btn = _invite_form(owner)
            email_box.fill(member_email)
            role_select.select_option("member")
            invite_btn.click()
            owner.get_by_text("Added existing user.", exact=True).wait_for(timeout=15_000)
            _wait_member(owner, ws, member_email, role="member")

            own_ws, _own_url = _login_ui(member, member_email, member_pw)
            self.assertNotEqual(own_ws, ws, "member must not land in the owner's workspace")
            _goto_settings(member, f"{UI}/{ws}")
            self.assertIn(ws, member.url)
            self.assertEqual(member.locator("h1").first.inner_text().strip(), "Settings")
            _member_row(member, member_email).first.wait_for(timeout=15_000)
            self.assertEqual(
                member.get_by_role("button", name="Invite", exact=True).count(),
                0,
                "member must not see Invite",
            )
            self.assertEqual(
                member.locator(
                    "input[type='email'][placeholder='teammate@example.test']"
                ).count(),
                0,
                "member must not see the invite email field",
            )
            self.assertEqual(
                member.get_by_role("button", name="Revoke", exact=True).count(),
                0,
                "member must not list or revoke invites",
            )
            peer_row = _member_row(member, member_email)
            self.assertEqual(
                peer_row.locator("select").count(),
                0,
                "member must not change roles",
            )

            forbidden = member.request.post(
                f"{UI}/api/workspaces/{ws}/invites",
                data=json.dumps(
                    {
                        "email": f"set-mem-nope-{uuid.uuid4().hex[:8]}@example.test",
                        "role": "member",
                    }
                ),
                headers={"Content-Type": "application/json"},
            )
            self.assertEqual(
                forbidden.status,
                403,
                f"member POST /invites must 403, got {forbidden.status}: "
                f"{forbidden.text()[:400]}",
            )
            after = _invites(owner, ws)
            self.assertEqual(
                after.get("invites") or [],
                [],
                f"member must not create an invite: {after}",
            )
        finally:
            member_ctx.close()
            owner_ctx.close()


if __name__ == "__main__":
    unittest.main(verbosity=2)
