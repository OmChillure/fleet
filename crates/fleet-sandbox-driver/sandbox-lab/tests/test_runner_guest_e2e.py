#!/usr/bin/env python3
"""Real-life runner guest e2e (layer 3).

Catalog: docs/components/runners/testing-requirements.md
Procedure: docs/components/runners/testing-procedures.md

ENABLE_RUNNER_GUEST_E2E defaults on for every PROFILE. Live guest
methods skip only when the flag is explicitly 0. Catalog-contract
tests always run.

When the flag is on: this-tree identity is required, and every platform
in PLATFORMS except LIVE_SKIPPED must complete the guest procedure or
the test fails. LIVE_SKIPPED is TCG ARM, freebsd-amd64, and Darwin
(no bootable guest on this host) — skipped for now; catalog still
lists them.
Do not WARN-and-continue. Do not invent a second install path.
"""
from __future__ import annotations

import http.cookiejar
import json
import os
import subprocess
import sys
import time
import unittest
import uuid
import urllib.request
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

_LAB_TESTS = Path(__file__).resolve().parent
sys.path.insert(0, str(_LAB_TESTS))
from lab_env import env_flag  # noqa: E402

REPO = Path(__file__).resolve().parents[4]
TREE_ID = REPO / "scripts" / "tree-id.sh"

# TCG ARM, freebsd hang, Darwin has no bootable image on this Linux
# host. Re-enable when they can finish the procedure. Catalog still
# lists the ids.
LIVE_SKIPPED = frozenset(
    {
        "linux-aarch64",
        "linux-aarch64-musl",
        "linux-armv7",
        "linux-armv7-musl",
        "freebsd-amd64",
        "darwin-x86_64",
        "darwin-arm64",
        "windows-arm64",
    }
)

API = os.environ.get("FLEET_API_URL", "http://127.0.0.1:8080").rstrip("/")
UI = os.environ.get("FLEET_UI_URL", "http://127.0.0.1:3030").rstrip("/")


def _guest_e2e_on() -> bool:
    return env_flag("ENABLE_RUNNER_GUEST_E2E", dotenv_path=REPO / ".env")


def _tree_id() -> str:
    import subprocess

    r = subprocess.run(
        ["bash", str(TREE_ID)],
        cwd=REPO,
        capture_output=True,
        text=True,
        check=False,
    )
    if r.returncode != 0:
        raise RuntimeError(f"scripts/tree-id.sh failed: {r.stderr}")
    return r.stdout.strip()


def _json(url: str, *, data: bytes | None = None, headers: dict | None = None) -> dict:
    req = Request(url, data=data, headers=headers or {}, method="GET" if data is None else "POST")
    with urlopen(req, timeout=8) as resp:
        raw = resp.read().decode()
    return json.loads(raw) if raw else {}


class RunnerGuestCatalogTests(unittest.TestCase):
    """Always on. Proves the procedure and matrix exist."""

    def test_flag_defaults_on(self) -> None:
        for profile in ("full", "lab", "minimal"):
            self.assertTrue(
                env_flag(
                    "ENABLE_RUNNER_GUEST_E2E",
                    environ={},
                    dotenv_text=f"PROFILE={profile}\n",
                ),
                f"PROFILE={profile} must default guest e2e on",
            )

    def test_flag_explicit_zero_wins(self) -> None:
        for profile in ("full", "lab", "minimal"):
            self.assertFalse(
                env_flag(
                    "ENABLE_RUNNER_GUEST_E2E",
                    environ={"ENABLE_RUNNER_GUEST_E2E": "1"},
                    dotenv_text=f"PROFILE={profile}\nENABLE_RUNNER_GUEST_E2E=0\n",
                ),
                f"explicit 0 must win over PROFILE={profile}",
            )


class RunnerGuestFailClosedContractTests(unittest.TestCase):
    """Always on. Missing required guests fail without booting a VM."""

    def test_live_skipped_guest_skips_not_fail(self) -> None:
        # Hardcoded ids — iterating LIVE_SKIPPED would pass if a guest
        # was deleted from both the set and this list.
        catalog_skipped = (
            "linux-aarch64",
            "linux-aarch64-musl",
            "linux-armv7",
            "linux-armv7-musl",
            "freebsd-amd64",
            "darwin-x86_64",
            "darwin-arm64",
            "windows-arm64",
        )
        case = RunnerGuestLiveTests("test_linux_x86_64")
        for plat in catalog_skipped:
            with self.subTest(plat=plat):
                with self.assertRaises(unittest.SkipTest) as ctx:
                    case._require_guest(plat)
                self.assertIn(plat, str(ctx.exception))


class RunnerGuestLiveTests(unittest.TestCase):
    """Live guests. Skip when the flag is off. Fail closed when on."""

    @classmethod
    def setUpClass(cls) -> None:
        if not _guest_e2e_on():
            raise unittest.SkipTest(
                "ENABLE_RUNNER_GUEST_E2E is off. Set it in repo-root .env "
                "and follow docs/components/runners/testing-procedures.md"
            )
        want = _tree_id()
        try:
            health = _json(f"{API}/health")
        except (URLError, HTTPError, TimeoutError, json.JSONDecodeError) as e:
            raise RuntimeError(
                f"API {API}/health is required when ENABLE_RUNNER_GUEST_E2E=1: {e}"
            ) from e
        sha = (health.get("git_sha") or "").strip()
        if sha != want:
            raise RuntimeError(
                f"/health.git_sha={sha!r} != this tree {want!r}. "
                "Rebuild this tree before guest e2e."
            )
        try:
            with urlopen(f"{UI}/fleet-git-sha.txt", timeout=5) as resp:
                ui_sha = resp.read().decode().strip()
        except (URLError, HTTPError, TimeoutError) as e:
            raise RuntimeError(
                f"UI {UI}/fleet-git-sha.txt is required when ENABLE_RUNNER_GUEST_E2E=1: {e}"
            ) from e
        if ui_sha != want:
            raise RuntimeError(
                f"UI /fleet-git-sha.txt={ui_sha!r} != this tree {want!r}."
            )

    def _default_guest_driver(self, platform: str) -> str:
        """Lab driver under `.tmp/vms/<platform>/` when one exists."""
        names = {
            "linux-x86_64": ("run-guest-procedure.py",),
            "linux-x86_64-musl": ("run-guest-e2e.py",),
            "linux-armv7": ("run-guest-procedure.py",),
            "windows-x86_64": ("guest-e2e-driver.py",),
        }
        for name in names.get(platform, ()):
            path = REPO / ".tmp" / "vms" / platform / name
            if path.is_file():
                return str(path)
        return ""

    def _require_guest(self, platform: str) -> None:
        """Fail until a guest for `platform` completes the procedure.

        Wire the guest driver here (`.tmp/vms/<platform>/`, qemu, ssh).
        ENABLE_RUNNER_GUEST_E2E=1 is an explicit fail when no guest
        driver exists so the hole cannot go green.
        """
        if platform in LIVE_SKIPPED:
            self.skipTest(
                f"guest {platform} skipped for now. "
                "Still listed in the catalog; re-enable LIVE_SKIPPED."
            )
        # Empty FLEET_RUNNER_GUEST_DRIVER is an explicit missing-guest
        # (fail-closed contract). Unset falls back to the lab driver.
        if "FLEET_RUNNER_GUEST_DRIVER" in os.environ:
            driver = os.environ.get("FLEET_RUNNER_GUEST_DRIVER", "").strip()
        else:
            driver = self._default_guest_driver(platform)
        if driver:
            self._run_driver(driver, platform)
            return
        self.fail(
            f"guest {platform} is required (ENABLE_RUNNER_GUEST_E2E=1). "
            "Boot the image, pair with this tree's install command, and "
            "walk docs/components/runners/testing-procedures.md "
            "(install, heartbeat, native job, apply/ack, update, "
            "offline 90s, machine.id reclaim, plane gate). "
            "Or set FLEET_RUNNER_GUEST_DRIVER to a script that does that "
            f"and prints OK {platform}."
        )

    def _run_driver(self, driver: str, platform: str) -> None:
        import subprocess

        r = subprocess.run(
            [driver, platform],
            cwd=REPO,
            capture_output=True,
            text=True,
        )
        self.assertEqual(
            r.returncode,
            0,
            f"{driver} {platform} failed:\n{r.stdout}\n{r.stderr}",
        )
        self.assertIn(
            f"OK {platform}",
            r.stdout,
            f"{driver} must print 'OK {platform}' when the procedure passes",
        )

    def _cookie_json(
        self,
        opener: urllib.request.OpenerDirector,
        method: str,
        url: str,
        body: dict | None = None,
        timeout: float = 15,
    ) -> dict:
        data = None
        headers = {"Accept": "application/json"}
        if body is not None:
            data = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
        req = urllib.request.Request(url, data=data, headers=headers, method=method)
        try:
            with opener.open(req, timeout=timeout) as resp:
                raw = resp.read().decode()
        except HTTPError as e:
            raw = e.read().decode() if e.fp else ""
            self.fail(f"{method} {url} -> {e.code}: {raw[:400]}")
        except (URLError, TimeoutError) as e:
            self.fail(f"{method} {url}: {e}")
        return json.loads(raw) if raw else {}

    def _pending_linux_x86_64_host(
        self,
    ) -> tuple[urllib.request.OpenerDirector, str, str, str]:
        """Owner cookie + pending host. Returns opener, workspace, host id, pair token."""
        opener = urllib.request.build_opener(
            urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar())
        )
        email = f"guest-linux-x86_64-{uuid.uuid4().hex[:12]}@example.test"
        signup: dict = {
            "email": email,
            "password": "correct-horse",
            "display_name": "guest-linux-x86_64",
        }
        invite = os.environ.get("SIGNUP_TOKEN", "").strip()
        if invite:
            signup["token"] = invite
        self._cookie_json(opener, "POST", f"{API}/api/auth/signup", signup)
        login = self._cookie_json(
            opener,
            "POST",
            f"{API}/api/auth/login",
            {"email": email, "password": "correct-horse"},
        )
        ws = str(((login.get("default_workspace") or {}).get("id") or ""))
        if not ws:
            me = self._cookie_json(opener, "GET", f"{API}/api/auth/me")
            spaces = me.get("workspaces") or []
            ws = str((spaces[0].get("id") if spaces else "") or "")
        self.assertTrue(ws, f"login returned no workspace: {login!r}"[:400])
        pending = self._cookie_json(
            opener,
            "POST",
            f"{API}/api/workspaces/{ws}/worker-hosts",
            {"name": f"linux-x86_64-{uuid.uuid4().hex[:8]}"},
        )
        hid = str(((pending.get("host") or {}).get("id") or ""))
        pair = str(pending.get("pair_token") or "")
        self.assertTrue(hid, f"create host missing id: {pending!r}"[:400])
        self.assertTrue(pair, f"create host missing pair_token: {pending!r}"[:400])
        return opener, ws, hid, pair

    def _pending_windows_x86_64_host(
        self,
    ) -> tuple[urllib.request.OpenerDirector, str, str, str]:
        """Owner cookie + pending host. Returns opener, workspace, host id, pair token."""
        opener = urllib.request.build_opener(
            urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar())
        )
        email = f"guest-windows-x86_64-{uuid.uuid4().hex[:12]}@example.test"
        signup: dict = {
            "email": email,
            "password": "correct-horse",
            "display_name": "guest-windows-x86_64",
        }
        invite = os.environ.get("SIGNUP_TOKEN", "").strip()
        if invite:
            signup["token"] = invite
        self._cookie_json(opener, "POST", f"{API}/api/auth/signup", signup)
        login = self._cookie_json(
            opener,
            "POST",
            f"{API}/api/auth/login",
            {"email": email, "password": "correct-horse"},
        )
        ws = str(((login.get("default_workspace") or {}).get("id") or ""))
        if not ws:
            me = self._cookie_json(opener, "GET", f"{API}/api/auth/me")
            spaces = me.get("workspaces") or []
            ws = str((spaces[0].get("id") if spaces else "") or "")
        self.assertTrue(ws, f"login returned no workspace: {login!r}"[:400])
        pending = self._cookie_json(
            opener,
            "POST",
            f"{API}/api/workspaces/{ws}/worker-hosts",
            {"name": f"windows-x86_64-{uuid.uuid4().hex[:8]}"},
        )
        hid = str(((pending.get("host") or {}).get("id") or ""))
        pair = str(pending.get("pair_token") or "")
        self.assertTrue(hid, f"create host missing id: {pending!r}"[:400])
        self.assertTrue(pair, f"create host missing pair_token: {pending!r}"[:400])
        return opener, ws, hid, pair

    def test_linux_x86_64(self) -> None:
        if "FLEET_RUNNER_GUEST_DRIVER" in os.environ:
            driver = os.environ.get("FLEET_RUNNER_GUEST_DRIVER", "").strip()
        else:
            driver = self._default_guest_driver("linux-x86_64")
        if not driver:
            self.fail(
                "guest linux-x86_64 is required (ENABLE_RUNNER_GUEST_E2E=1). "
                "Boot the image, pair with this tree's install command, and "
                "walk docs/components/runners/testing-procedures.md "
                "(install, heartbeat, native job, apply/ack, update, "
                "offline 90s, machine.id reclaim, plane gate). "
                "Or set FLEET_RUNNER_GUEST_DRIVER to a script that does that "
                "using FLEET_PAIR_TOKEN from this test."
            )
        opener, ws, hid, pair = self._pending_linux_x86_64_host()
        env = os.environ.copy()
        env["FLEET_PAIR_TOKEN"] = pair
        env["FLEET_PLATFORM"] = "linux-x86_64"
        env["FLEET_URL"] = (
            os.environ.get("WORKER_PUBLIC_BASE", "").strip().rstrip("/") or API
        )
        ran = subprocess.run(
            [driver, "linux-x86_64"],
            cwd=REPO,
            env=env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(
            ran.returncode,
            0,
            f"{driver} linux-x86_64 failed:\n{ran.stdout}\n{ran.stderr}",
        )
        listed = self._cookie_json(
            opener, "GET", f"{API}/api/workspaces/{ws}/worker-hosts"
        )
        host = next(
            (
                row
                for row in (listed.get("hosts") or [])
                if str(row.get("id") or "") == hid
            ),
            {},
        )
        self.assertEqual(
            host.get("id"),
            hid,
            f"GET worker-hosts missing {hid}: {listed!r}"[:800],
        )
        self.assertEqual(host.get("status"), "online")
        self.assertEqual(host.get("platform"), "linux-x86_64")
        self.assertEqual(host.get("isolation"), "native")
        self.assertTrue(host.get("paired_at"), f"paired_at: {host!r}"[:400])
        self.assertTrue(host.get("last_seen_at"), f"last_seen_at: {host!r}"[:400])
        self.assertTrue(host.get("machine_id"), f"machine_id: {host!r}"[:400])
        self.assertTrue(host.get("advertised_host"), f"advertised_host: {host!r}"[:400])

    def test_linux_x86_64_musl(self) -> None:
        self._require_guest("linux-x86_64-musl")

    def test_linux_armv7(self) -> None:
        # First-class served platform (linux-armv7.sh / stage --armv7).
        # LIVE_SKIPPED until a TCG/qemu guest exists; a driver must still
        # be able to fail-close this id (skip must not run first).
        if "FLEET_RUNNER_GUEST_DRIVER" in os.environ:
            driver = os.environ.get("FLEET_RUNNER_GUEST_DRIVER", "").strip()
        else:
            driver = self._default_guest_driver("linux-armv7")
        if driver:
            self._run_driver(driver, "linux-armv7")
            return
        self._require_guest("linux-armv7")

    def test_windows_x86_64(self) -> None:
        if "FLEET_RUNNER_GUEST_DRIVER" in os.environ:
            driver = os.environ.get("FLEET_RUNNER_GUEST_DRIVER", "").strip()
        else:
            driver = self._default_guest_driver("windows-x86_64")
        if not driver:
            self.fail(
                "guest windows-x86_64 is required (ENABLE_RUNNER_GUEST_E2E=1). "
                "Boot the image, run this plane's PowerShell install-worker.ps1 "
                "command, and walk docs/components/runners/testing-procedures.md "
                "(install, heartbeat, native job, apply/ack, update, "
                "offline 90s, machine.id reclaim, plane gate). "
                "Or set FLEET_RUNNER_GUEST_DRIVER to a script that runs "
                "FLEET_INSTALL_CMD (the served irm | iex one-liner) on the guest "
                "using FLEET_PAIR_TOKEN from this test."
            )
        opener, ws, hid, pair = self._pending_windows_x86_64_host()
        worker_url = (
            os.environ.get("WORKER_PUBLIC_BASE", "").strip().rstrip("/") or API
        )
        try:
            with urlopen(f"{worker_url}/install-worker.ps1", timeout=8) as resp:
                served = resp.read()
        except (URLError, HTTPError, TimeoutError) as e:
            self.fail(f"GET {worker_url}/install-worker.ps1: {e}")
        self.assertTrue(served, f"{worker_url}/install-worker.ps1 is empty")
        install_cmd = (
            f"$env:FLEET_URL='{worker_url}'; "
            f"$env:FLEET_PAIR_TOKEN='{pair}'; "
            'irm "$env:FLEET_URL/install-worker.ps1" | iex'
        )
        env = os.environ.copy()
        env["FLEET_PAIR_TOKEN"] = pair
        env["FLEET_PLATFORM"] = "windows-x86_64"
        env["FLEET_URL"] = worker_url
        env["FLEET_INSTALL_CMD"] = install_cmd
        ran = subprocess.run(
            [driver, "windows-x86_64"],
            cwd=REPO,
            env=env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(
            ran.returncode,
            0,
            f"{driver} windows-x86_64 install failed:\n{ran.stdout}\n{ran.stderr}",
        )
        listed = self._cookie_json(
            opener, "GET", f"{API}/api/workspaces/{ws}/worker-hosts"
        )
        host = next(
            (
                row
                for row in (listed.get("hosts") or [])
                if str(row.get("id") or "") == hid
            ),
            {},
        )
        self.assertEqual(
            host.get("id"),
            hid,
            f"GET worker-hosts missing {hid}: {listed!r}"[:800],
        )
        self.assertEqual(host.get("status"), "online")
        self.assertEqual(host.get("platform"), "windows-x86_64")
        self.assertEqual(host.get("isolation"), "native")
        self.assertTrue(host.get("paired_at"), f"paired_at: {host!r}"[:400])
        self.assertTrue(host.get("last_seen_at"), f"last_seen_at: {host!r}"[:400])
        mid = host.get("machine_id")
        self.assertTrue(mid, f"machine_id: {host!r}"[:400])
        self.assertTrue(host.get("advertised_host"), f"advertised_host: {host!r}"[:400])

        self._cookie_json(
            opener,
            "POST",
            f"{API}/api/workspaces/{ws}/fleet/runs/fixture",
            {"kind": "chain"},
        )
        self._cookie_json(
            opener,
            "POST",
            f"{API}/api/workspaces/{ws}/fleet/tick",
            timeout=60,
        )
        deadline = time.monotonic() + 90
        leased = {}
        while time.monotonic() < deadline:
            listed = self._cookie_json(
                opener, "GET", f"{API}/api/workspaces/{ws}/worker-hosts"
            )
            leased = next(
                (
                    row
                    for row in (listed.get("hosts") or [])
                    if str(row.get("id") or "") == hid
                ),
                {},
            )
            if leased.get("current_jobs") == 1 and leased.get("isolation") == "native":
                break
            time.sleep(2)
        self.assertEqual(
            leased.get("current_jobs"),
            1,
            f"guest did not take one native lease: {leased!r}"[:800],
        )
        self.assertEqual(leased.get("isolation"), "native")
        sessions = self._cookie_json(
            opener, "GET", f"{API}/api/workspaces/{ws}/fleet/sessions"
        )
        native = [
            row
            for row in (sessions.get("sessions") or [])
            if str(row.get("runtime_handle") or "") == f"host:{hid}"
            and str(row.get("runtime_backend") or "") != "container"
        ]
        self.assertTrue(
            native,
            f"GET fleet/sessions missing native lease on {hid}: {sessions!r}"[:800],
        )

        self._cookie_json(
            opener, "DELETE", f"{API}/api/workspaces/{ws}/worker-hosts/{hid}"
        )
        pending2 = self._cookie_json(
            opener,
            "POST",
            f"{API}/api/workspaces/{ws}/worker-hosts",
            {"name": f"windows-x86_64-{uuid.uuid4().hex[:8]}"},
        )
        hid2 = str(((pending2.get("host") or {}).get("id") or ""))
        pair2 = str(pending2.get("pair_token") or "")
        self.assertTrue(hid2, f"reclaim create host missing id: {pending2!r}"[:400])
        self.assertTrue(
            pair2, f"reclaim create host missing pair_token: {pending2!r}"[:400]
        )
        self.assertNotEqual(hid2, hid)
        install_cmd2 = (
            f"$env:FLEET_URL='{worker_url}'; "
            f"$env:FLEET_PAIR_TOKEN='{pair2}'; "
            'irm "$env:FLEET_URL/install-worker.ps1" | iex'
        )
        env2 = os.environ.copy()
        env2["FLEET_PAIR_TOKEN"] = pair2
        env2["FLEET_PLATFORM"] = "windows-x86_64"
        env2["FLEET_URL"] = worker_url
        env2["FLEET_INSTALL_CMD"] = install_cmd2
        ran2 = subprocess.run(
            [driver, "windows-x86_64"],
            cwd=REPO,
            env=env2,
            capture_output=True,
            text=True,
        )
        self.assertEqual(
            ran2.returncode,
            0,
            f"{driver} windows-x86_64 reclaim install failed:\n{ran2.stdout}\n{ran2.stderr}",
        )
        listed2 = self._cookie_json(
            opener, "GET", f"{API}/api/workspaces/{ws}/worker-hosts"
        )
        reclaimed = next(
            (
                row
                for row in (listed2.get("hosts") or [])
                if str(row.get("id") or "") == hid
            ),
            {},
        )
        self.assertEqual(
            reclaimed.get("id"),
            hid,
            f"machine.id reclaim missing {hid}: {listed2!r}"[:800],
        )
        self.assertEqual(reclaimed.get("status"), "online")
        self.assertEqual(reclaimed.get("platform"), "windows-x86_64")
        self.assertEqual(reclaimed.get("machine_id"), mid)
        clone = next(
            (
                row
                for row in (listed2.get("hosts") or [])
                if str(row.get("id") or "") == hid2
            ),
            None,
        )
        if clone is not None:
            self.assertEqual(clone.get("status"), "disabled")


if __name__ == "__main__":
    unittest.main(verbosity=2)
