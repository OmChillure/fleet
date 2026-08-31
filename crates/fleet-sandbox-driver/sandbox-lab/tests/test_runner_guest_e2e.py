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

import inspect
import json
import os
import sys
import unittest
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

_LAB_TESTS = Path(__file__).resolve().parent
sys.path.insert(0, str(_LAB_TESTS))
from lab_env import env_flag  # noqa: E402

REPO = Path(__file__).resolve().parents[4]
PROCEDURES = REPO / "docs" / "components" / "runners" / "testing-procedures.md"
REQUIREMENTS = REPO / "docs" / "components" / "runners" / "testing-requirements.md"
INSTALL_SH = REPO / "scripts" / "install-worker" / "install-worker.sh"
TREE_ID = REPO / "scripts" / "tree-id.sh"

PLATFORMS = (
    "linux-x86_64",
    "linux-x86_64-musl",
    "linux-x86_64-bookworm",
    "linux-aarch64",
    "linux-aarch64-musl",
    "linux-armv7",
    "linux-armv7-musl",
    "freebsd-amd64",
    "darwin-x86_64",
    "darwin-arm64",
    "windows-x86_64",
    "windows-arm64",
)

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

    def test_procedures_and_requirements_exist(self) -> None:
        self.assertTrue(REQUIREMENTS.is_file(), REQUIREMENTS)
        self.assertTrue(PROCEDURES.is_file(), PROCEDURES)
        req = REQUIREMENTS.read_text(encoding="utf-8")
        proc = PROCEDURES.read_text(encoding="utf-8")
        self.assertIn("Three layers", req)
        self.assertIn("Real-life e2e", req)
        self.assertIn("ENABLE_RUNNER_GUEST_E2E", proc)
        self.assertIn("Per-platform procedure", proc)
        for plat in PLATFORMS:
            self.assertIn(plat, req, f"requirements missing {plat}")
            self.assertIn(plat, proc, f"procedures missing {plat}")
        for plat in LIVE_SKIPPED:
            self.assertIn(plat, req)
            self.assertIn("Skipped for now", proc)
            self.assertIn("Live skipped for now", req)

    def test_install_scripts_take_pair_token(self) -> None:
        unix = (REPO / "scripts" / "install-worker" / "_unix_common.sh").read_text(
            encoding="utf-8"
        )
        win = (REPO / "scripts" / "install-worker" / "windows-x86_64.ps1").read_text(
            encoding="utf-8"
        )
        self.assertIn("FLEET_PAIR_TOKEN", unix)
        self.assertIn("FLEET_URL", INSTALL_SH.read_text(encoding="utf-8"))
        self.assertIn("FLEET_PAIR_TOKEN", win)
        self.assertIn("FLEET_URL", win)

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

    def test_required_guests_are_not_live_skipped(self) -> None:
        for plat in (
            "linux-x86_64",
            "linux-x86_64-musl",
            "linux-x86_64-bookworm",
            "windows-x86_64",
        ):
            self.assertIn(plat, PLATFORMS, plat)
            self.assertNotIn(plat, LIVE_SKIPPED, plat)

    def test_live_methods_exist_for_every_platform(self) -> None:
        for plat in PLATFORMS:
            name = "test_" + plat.replace("-", "_")
            self.assertTrue(
                hasattr(RunnerGuestLiveTests, name),
                f"RunnerGuestLiveTests.{name} missing for {plat}",
            )

    def test_missing_required_guest_fails_not_warn(self) -> None:
        required = tuple(p for p in PLATFORMS if p not in LIVE_SKIPPED)
        self.assertIn("linux-x86_64", required)
        self.assertIn("windows-x86_64", required)
        prev = os.environ.get("FLEET_RUNNER_GUEST_DRIVER")
        os.environ["FLEET_RUNNER_GUEST_DRIVER"] = ""
        try:
            case = RunnerGuestLiveTests("test_linux_x86_64")
            for plat in required:
                with self.subTest(plat=plat):
                    with self.assertRaises(AssertionError) as ctx:
                        case._require_guest(plat)
                    msg = str(ctx.exception)
                    self.assertIn(plat, msg)
                    self.assertIn("ENABLE_RUNNER_GUEST_E2E=1", msg)
                    self.assertNotIn("WARN", msg)
        finally:
            if prev is None:
                os.environ.pop("FLEET_RUNNER_GUEST_DRIVER", None)
            else:
                os.environ["FLEET_RUNNER_GUEST_DRIVER"] = prev

    def test_live_skipped_guest_skips_not_fail(self) -> None:
        case = RunnerGuestLiveTests("test_linux_aarch64")
        self.assertTrue(LIVE_SKIPPED)
        for plat in LIVE_SKIPPED:
            with self.subTest(plat=plat):
                with self.assertRaises(unittest.SkipTest) as ctx:
                    case._require_guest(plat)
                msg = str(ctx.exception)
                self.assertIn(plat, msg)
                self.assertIn("skipped for now", msg)

    def test_live_skipped_guests_remain_in_catalog(self) -> None:
        # Hardcoded ids — iterating LIVE_SKIPPED would pass if a guest
        # was deleted from both the set and the catalog.
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
        req = REQUIREMENTS.read_text(encoding="utf-8")
        proc = PROCEDURES.read_text(encoding="utf-8")
        for plat in catalog_skipped:
            with self.subTest(plat=plat, part="catalog"):
                self.assertIn(plat, PLATFORMS, f"{plat} dropped from PLATFORMS")
                self.assertIn(plat, LIVE_SKIPPED, f"{plat} must stay LIVE_SKIPPED")
                self.assertIn(plat, req, f"requirements catalog dropped {plat}")
                self.assertIn(plat, proc, f"procedures catalog dropped {plat}")
                name = "test_" + plat.replace("-", "_")
                self.assertTrue(
                    hasattr(RunnerGuestLiveTests, name),
                    f"RunnerGuestLiveTests.{name} was deleted; skip, do not drop",
                )
        self.assertIn("Live skipped for now", req)
        self.assertIn("Skipped for now", proc)

        required = ("linux-x86_64", "windows-x86_64")
        prev = os.environ.get("FLEET_RUNNER_GUEST_DRIVER")
        os.environ["FLEET_RUNNER_GUEST_DRIVER"] = ""
        try:
            case = RunnerGuestLiveTests("test_linux_x86_64")
            for plat in required:
                with self.subTest(plat=plat, part="required"):
                    self.assertIn(plat, PLATFORMS, plat)
                    self.assertNotIn(plat, LIVE_SKIPPED, plat)
                    with self.assertRaises(AssertionError) as ctx:
                        case._require_guest(plat)
                    msg = str(ctx.exception)
                    self.assertIn(plat, msg)
                    self.assertIn("ENABLE_RUNNER_GUEST_E2E=1", msg)
                    self.assertNotIn("WARN", msg)
        finally:
            if prev is None:
                os.environ.pop("FLEET_RUNNER_GUEST_DRIVER", None)
            else:
                os.environ["FLEET_RUNNER_GUEST_DRIVER"] = prev

    def test_explicit_zero_skips_live_setup(self) -> None:
        real = _guest_e2e_on
        globals()["_guest_e2e_on"] = lambda: False
        try:
            with self.assertRaises(unittest.SkipTest) as ctx:
                RunnerGuestLiveTests.setUpClass()
            self.assertIn("ENABLE_RUNNER_GUEST_E2E is off", str(ctx.exception))
        finally:
            globals()["_guest_e2e_on"] = real

    def test_catalog_tests_always_run(self) -> None:
        src = inspect.getsource(RunnerGuestCatalogTests)
        self.assertNotIn("SkipTest", src)
        self.assertNotIn("_guest_e2e_on", src)

    def test_require_guest_source_is_fail_closed(self) -> None:
        src = inspect.getsource(RunnerGuestLiveTests._require_guest)
        self.assertIn("self.fail(", src)
        self.assertNotIn("WARN", src)
        self.assertIn("LIVE_SKIPPED", src)


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
            "linux-x86_64-bookworm": ("driver.sh",),
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

    def test_linux_x86_64(self) -> None:
        self._require_guest("linux-x86_64")

    def test_linux_x86_64_musl(self) -> None:
        self._require_guest("linux-x86_64-musl")

    def test_linux_x86_64_bookworm(self) -> None:
        self._require_guest("linux-x86_64-bookworm")

    def test_linux_aarch64(self) -> None:
        self._require_guest("linux-aarch64")

    def test_linux_aarch64_musl(self) -> None:
        self._require_guest("linux-aarch64-musl")

    def test_linux_armv7(self) -> None:
        self._require_guest("linux-armv7")

    def test_linux_armv7_musl(self) -> None:
        self._require_guest("linux-armv7-musl")

    def test_freebsd_amd64(self) -> None:
        self._require_guest("freebsd-amd64")

    def test_darwin_x86_64(self) -> None:
        self._require_guest("darwin-x86_64")

    def test_darwin_arm64(self) -> None:
        self._require_guest("darwin-arm64")

    def test_windows_x86_64(self) -> None:
        self._require_guest("windows-x86_64")

    def test_windows_arm64(self) -> None:
        self._require_guest("windows-arm64")


if __name__ == "__main__":
    unittest.main(verbosity=2)
