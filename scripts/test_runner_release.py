#!/usr/bin/env python3
"""Unit tests for scripts/runner-release.py (no docker)."""
from __future__ import annotations

import hashlib
import json
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
import importlib.util

_spec = importlib.util.spec_from_file_location(
    "runner_release", ROOT / "scripts" / "runner-release.py"
)
rr = importlib.util.module_from_spec(_spec)
assert _spec.loader is not None
_spec.loader.exec_module(rr)


def _sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


class RunnerReleaseTests(unittest.TestCase):
    def test_check_fails_when_staged_host_hash_drifts(self) -> None:
        """The ab04bca deploy: rebuilt bins, lock still 0.0.3 old hashes."""
        dest = Path(tempfile.mkdtemp(prefix="rr-"))
        self.addCleanup(lambda: shutil.rmtree(dest, ignore_errors=True))
        locked = _sha(b"locked-host")
        drifted = _sha(b"rebuilt-host")
        driver = _sha(b"driver")
        host_kids = {p: locked for p in rr.SHIP_PLATFORMS}
        drv_kids = {p: driver for p in rr.SHIP_PLATFORMS}
        text = (
            "[versions]\nfleet-host = \"0.0.3\"\nfleet-driver = \"0.0.3\"\n"
            "[verify]\nfleet-host = \"0.0.3\"\nfleet-driver = \"0.0.3\"\n"
            "[release.fleet-host]\n"
            f'version = "0.0.3"\nparent = "{rr.parent_digest(host_kids)}"\n'
            f'linux-x86_64 = "{locked}"\nlinux-x86_64-rev = 0\n'
            f'linux-aarch64 = "{locked}"\nlinux-aarch64-rev = 0\n'
            f'windows-x86_64 = "{locked}"\nwindows-x86_64-rev = 0\n'
            "[release.fleet-driver]\n"
            f'version = "0.0.3"\nparent = "{rr.parent_digest(drv_kids)}"\n'
            f'linux-x86_64 = "{driver}"\nlinux-x86_64-rev = 0\n'
            f'linux-aarch64 = "{driver}"\nlinux-aarch64-rev = 0\n'
            f'windows-x86_64 = "{driver}"\nwindows-x86_64-rev = 0\n'
        )
        toml_path = dest / "runner-packages.toml"
        toml_path.write_text(text)
        plat = dest / "dist" / "linux-x86_64"
        plat.mkdir(parents=True)
        (plat / "manifest.json").write_text(
            json.dumps(
                {
                    "git_sha": "ab04bca26939e05040c093d95de83b9dc9562561",
                    "bins": {
                        "fleet-host": {"sha256": drifted},
                        "fleet-driver": {"sha256": driver},
                    },
                }
            )
        )
        rc = rr.cmd_check(toml_path, dest / "dist", require_all=False)
        self.assertEqual(rc, 1)
        self.assertEqual(rr.cmd_platform_ok(toml_path, dest / "dist", "linux-x86_64"), 1)

    def test_register_bumps_only_changed_os_rev(self) -> None:
        dest = Path(tempfile.mkdtemp(prefix="rr-"))
        self.addCleanup(lambda: shutil.rmtree(dest, ignore_errors=True))
        old_h = _sha(b"old-host")
        new_h = _sha(b"new-host")
        drv = _sha(b"drv")
        children = {
            "linux-x86_64": old_h,
            "linux-aarch64": old_h,
            "windows-x86_64": old_h,
        }
        text = (
            "[versions]\nfleet-host = \"0.0.3\"\nfleet-driver = \"0.0.3\"\n"
            "[release.fleet-host]\n"
            f'version = "0.0.3"\nparent = "{rr.parent_digest(children)}"\n'
            f'linux-x86_64 = "{old_h}"\nlinux-x86_64-rev = 0\n'
            f'linux-aarch64 = "{old_h}"\nlinux-aarch64-rev = 0\n'
            f'windows-x86_64 = "{old_h}"\nwindows-x86_64-rev = 0\n'
            "[release.fleet-driver]\n"
            f'version = "0.0.3"\nparent = "{rr.parent_digest({p: drv for p in children})}"\n'
            f'linux-x86_64 = "{drv}"\nlinux-x86_64-rev = 0\n'
            f'linux-aarch64 = "{drv}"\nlinux-aarch64-rev = 0\n'
            f'windows-x86_64 = "{drv}"\nwindows-x86_64-rev = 0\n'
        )
        toml_path = dest / "runner-packages.toml"
        toml_path.write_text(text)
        for plat, host_sha in (
            ("linux-x86_64", new_h),
            ("linux-aarch64", old_h),
            ("windows-x86_64", old_h),
        ):
            d = dest / "dist" / plat
            d.mkdir(parents=True)
            (d / "manifest.json").write_text(
                json.dumps(
                    {
                        "bins": {
                            "fleet-host": {"sha256": host_sha},
                            "fleet-driver": {"sha256": drv},
                        }
                    }
                )
            )
        # register reads toml next to the script in production; call cmd_register
        # after pointing it at dest/dist by writing the same structure.
        # cmd_register uses the given dest for staged hashes and toml_path for rewrite.
        rc = rr.cmd_register(toml_path, dest / "dist")
        self.assertEqual(rc, 0)
        rel = rr.parse_bin_release(toml_path.read_text(), "fleet-host")
        self.assertEqual(rel.revs["linux-x86_64"], 1)
        self.assertEqual(rel.revs["linux-aarch64"], 0)
        self.assertEqual(rel.children["linux-x86_64"], new_h)


if __name__ == "__main__":
    unittest.main()
