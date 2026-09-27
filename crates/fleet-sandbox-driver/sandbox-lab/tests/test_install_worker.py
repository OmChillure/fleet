#!/usr/bin/env python3
"""Worker bootstrap: POSIX syntax + platform detect. No network."""
from __future__ import annotations

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

# tests/ → sandbox-lab → fleet-sandbox-driver → crates → repo
REPO = Path(__file__).resolve().parents[4]
SCRIPT_DIR = REPO / "scripts" / "install-worker"
BOOTSTRAP = SCRIPT_DIR / "install-worker.sh"

PLATFORMS = (
    "linux-x86_64.sh",
    "linux-aarch64.sh",
    "linux-armv7.sh",
    "linux-x86_64-musl.sh",
    "linux-aarch64-musl.sh",
    "linux-armv7-musl.sh",
    "darwin-arm64.sh",
    "darwin-x86_64.sh",
    "freebsd-amd64.sh",
    "windows-x86_64.ps1",
    "windows-arm64.ps1",
    "install-worker.ps1",
    "_unix_common.sh",
)

# Documented dest ids for this host's uname/ldd (install-worker.sh detect_platform).
_HOST_UNAME_TO_ID = {
    ("Linux", "x86_64", False): "linux-x86_64",
    ("Linux", "amd64", False): "linux-x86_64",
    ("Linux", "x86_64", True): "linux-x86_64-musl",
    ("Linux", "amd64", True): "linux-x86_64-musl",
    ("Linux", "aarch64", False): "linux-aarch64",
    ("Linux", "arm64", False): "linux-aarch64",
    ("Linux", "aarch64", True): "linux-aarch64-musl",
    ("Linux", "arm64", True): "linux-aarch64-musl",
    ("Linux", "armv7l", False): "linux-armv7",
    ("Linux", "armv7", False): "linux-armv7",
    ("Linux", "armhf", False): "linux-armv7",
    ("Linux", "armv7l", True): "linux-armv7-musl",
    ("Linux", "armv7", True): "linux-armv7-musl",
    ("Linux", "armhf", True): "linux-armv7-musl",
    ("Darwin", "arm64", False): "darwin-arm64",
    ("Darwin", "aarch64", False): "darwin-arm64",
    ("Darwin", "x86_64", False): "darwin-x86_64",
    ("FreeBSD", "amd64", False): "freebsd-amd64",
    ("FreeBSD", "x86_64", False): "freebsd-amd64",
}


def _host_linux_musl() -> bool:
    if Path("/etc/alpine-release").is_file():
        return True
    try:
        r = subprocess.run(["ldd", "/bin/sh"], capture_output=True, text=True)
    except OSError:
        return False
    return "musl" in f"{r.stdout}{r.stderr}".lower()


def _expected_host_catalog_id() -> str:
    os_name = subprocess.check_output(["uname", "-s"], text=True).strip()
    machine = subprocess.check_output(["uname", "-m"], text=True).strip()
    musl = os_name == "Linux" and _host_linux_musl()
    try:
        return _HOST_UNAME_TO_ID[(os_name, machine, musl)]
    except KeyError as exc:
        raise AssertionError(
            f"no catalog id for host uname {os_name} {machine} musl={musl}"
        ) from exc


def _write_fake_uname_ldd(
    bindir: Path, os_name: str, machine: str, *, musl: bool = False
) -> None:
    (bindir / "uname").write_text(
        "#!/bin/sh\n"
        "case $1 in\n"
        f"  -s) echo {os_name} ;;\n"
        f"  -m) echo {machine} ;;\n"
        "  -r) echo 6.1.0 ;;\n"
        "  *) echo unknown ;;\n"
        "esac\n"
    )
    ldd = "/lib/ld-musl-x86_64.so.1" if musl else "/lib64/ld-linux-x86-64.so.2"
    (bindir / "ldd").write_text(f"#!/bin/sh\necho '{ldd}'\n")
    (bindir / "uname").chmod(0o755)
    (bindir / "ldd").chmod(0o755)


def _detect_only(
    *,
    bindir: Path | None = None,
    pop: tuple[str, ...] = (),
    stdin=None,
) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env["FLEET_DETECT_ONLY"] = "1"
    for key in pop:
        env.pop(key, None)
    if bindir is not None:
        env["PATH"] = f"{bindir}{os.pathsep}{env.get('PATH', '')}"
    return subprocess.run(
        ["sh", str(BOOTSTRAP)],
        capture_output=True,
        text=True,
        env=env,
        stdin=stdin,
    )


class InstallWorkerTests(unittest.TestCase):
    def test_posix_syntax(self):
        for name in (
            "install-worker.sh",
            "_unix_common.sh",
            "linux-x86_64.sh",
            "linux-aarch64.sh",
            "linux-armv7.sh",
            "linux-x86_64-musl.sh",
            "linux-aarch64-musl.sh",
            "linux-armv7-musl.sh",
            "darwin-arm64.sh",
            "darwin-x86_64.sh",
            "freebsd-amd64.sh",
            "verify.sh",
        ):
            r = subprocess.run(
                ["sh", "-n", str(SCRIPT_DIR / name)],
                capture_output=True,
                text=True,
            )
            self.assertEqual(r.returncode, 0, f"{name}: {r.stderr}")

    def test_detect_only_prints_known_id(self):
        """Host uname/ldd must print this machine's catalog id (not any of 11)."""
        want = _expected_host_catalog_id()
        r = _detect_only()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, want + "\n", r.stdout)
        dest = SCRIPT_DIR / f"{want}.sh"
        if not dest.is_file():
            dest = SCRIPT_DIR / f"{want}.ps1"
        self.assertTrue(dest.is_file(), want)

    def test_detect_only_injected_uname_prints_exact_catalog_id(self):
        """PATH-injected uname/ldd must print the exact catalog token."""
        cases = (
            ("Linux", "x86_64", False, "linux-x86_64"),
            ("Linux", "x86_64", True, "linux-x86_64-musl"),
            ("Darwin", "arm64", False, "darwin-arm64"),
            ("FreeBSD", "amd64", False, "freebsd-amd64"),
        )
        for os_name, machine, musl, want in cases:
            with self.subTest(os_name=os_name, machine=machine, musl=musl):
                with tempfile.TemporaryDirectory() as td:
                    bindir = Path(td)
                    _write_fake_uname_ldd(bindir, os_name, machine, musl=musl)
                    r = _detect_only(bindir=bindir)
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertEqual(r.stdout, want + "\n", r.stdout)

    def test_detect_only_unsupported_os_or_cpu_dies(self):
        """Unknown os/cpu must die; DETECT_ONLY must not print a catalog id."""
        cases = (
            ("SunOS", "x86_64", False, ("unsupported uname -s: SunOS",)),
            ("Linux", "i686", False, ("unsupported uname -m: i686",)),
            (
                "Linux",
                "riscv64",
                False,
                (
                    "no glibc installer for riscv64",
                    "no musl installer for riscv64",
                ),
            ),
        )
        for os_name, machine, musl, needles in cases:
            with self.subTest(os_name=os_name, machine=machine):
                with tempfile.TemporaryDirectory() as td:
                    bindir = Path(td)
                    _write_fake_uname_ldd(bindir, os_name, machine, musl=musl)
                    r = _detect_only(bindir=bindir)
                self.assertNotEqual(r.returncode, 0, r.stdout + r.stderr)
                self.assertEqual(r.stdout.strip(), "")
                err = r.stderr or ""
                self.assertTrue(any(n in err for n in needles), err)

    def test_detect_only_exits_before_prompt_when_fleet_url_unset(self):
        """DETECT_ONLY short-circuits prompt_origin even with no URL and piped stdin."""
        want = _expected_host_catalog_id()
        r = _detect_only(
            pop=("FLEET_URL", "FLEET_OFFICIAL_BASE"),
            stdin=subprocess.DEVNULL,
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, want + "\n", r.stdout)
        self.assertNotIn("FLEET_URL", (r.stderr or "") + (r.stdout or ""))

    def test_detect_armv7_musl_prints_id_not_die(self):
        """Alpine armv7l + musl must print linux-armv7-musl, not die."""
        import tempfile

        with tempfile.TemporaryDirectory() as td:
            bindir = Path(td)
            (bindir / "uname").write_text(
                "#!/bin/sh\n"
                "case $1 in\n"
                "  -s) echo Linux ;;\n"
                "  -m) echo armv7l ;;\n"
                "  -r) echo 6.12.13-0-virt ;;\n"
                "  *) echo unknown ;;\n"
                "esac\n"
            )
            (bindir / "ldd").write_text("#!/bin/sh\necho '/lib/ld-musl-armhf.so.1'\n")
            (bindir / "uname").chmod(0o755)
            (bindir / "ldd").chmod(0o755)
            env = os.environ.copy()
            env["FLEET_DETECT_ONLY"] = "1"
            env["PATH"] = f"{bindir}{os.pathsep}{env.get('PATH', '')}"
            r = subprocess.run(
                ["sh", str(BOOTSTRAP)],
                capture_output=True,
                text=True,
                env=env,
            )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), "linux-armv7-musl")
        self.assertNotIn("no musl installer for", r.stderr)

    def test_detect_linux_armv7_glibc_prints_id(self):
        """Linux armv7l + glibc ldd (not musl) must print linux-armv7."""
        with tempfile.TemporaryDirectory() as td:
            bindir = Path(td)
            (bindir / "uname").write_text(
                "#!/bin/sh\n"
                "case $1 in\n"
                "  -s) echo Linux ;;\n"
                "  -m) echo armv7l ;;\n"
                "  -r) echo 6.12.13-v7+ ;;\n"
                "  *) echo unknown ;;\n"
                "esac\n"
            )
            (bindir / "ldd").write_text("#!/bin/sh\necho '/lib/ld-linux-armhf.so.3'\n")
            (bindir / "uname").chmod(0o755)
            (bindir / "ldd").chmod(0o755)
            env = os.environ.copy()
            env["FLEET_DETECT_ONLY"] = "1"
            env["PATH"] = f"{bindir}{os.pathsep}{env.get('PATH', '')}"
            r = subprocess.run(
                ["sh", str(BOOTSTRAP)],
                capture_output=True,
                text=True,
                env=env,
            )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), "linux-armv7")
        self.assertNotIn("no glibc installer for", r.stderr)

    def test_piped_without_url_fails_closed(self):
        """Non-TTY sh with no FLEET_URL dies before a defaulted origin."""
        env = os.environ.copy()
        env.pop("FLEET_URL", None)
        env.pop("FLEET_OFFICIAL_BASE", None)
        env.pop("FLEET_DETECT_ONLY", None)
        r = subprocess.run(
            ["sh", str(BOOTSTRAP)],
            capture_output=True,
            text=True,
            env=env,
            stdin=subprocess.DEVNULL,
        )
        self.assertNotEqual(r.returncode, 0)
        err = (r.stderr or "") + (r.stdout or "")
        self.assertIn("FLEET_URL", err)
        self.assertNotIn("source:", err)
        self.assertNotIn("platform:", err)
        self.assertNotIn("refusing to pair", err)
        self.assertNotIn("is loopback", err)

    def test_ps1_ascii_only_for_windows_powershell_51(self):
        """PS 5.1 `powershell -File` on UTF-8 no-BOM treats U+2014 as a string terminator."""
        for name in ("install-worker.ps1", "windows-x86_64.ps1", "windows-arm64.ps1"):
            raw = (SCRIPT_DIR / name).read_bytes()
            non_ascii = [i for i, b in enumerate(raw) if b >= 128]
            self.assertEqual(
                non_ascii,
                [],
                f"{name}: non-ASCII bytes at {non_ascii[:8]} (PS 5.1 -File / CP-1252)",
            )
            self.assertNotIn("\u2014".encode("utf-8"), raw, f"{name}: em-dash U+2014")
            self.assertFalse(raw.startswith(b"\xef\xbb\xbf"), f"{name}: unexpected UTF-8 BOM")

    def test_stage_write_manifest_for_existing_dir(self):
        """--write-manifest hashes already-staged bins without rebuilding."""
        import json

        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            linux = root / "linux-x86_64"
            win = root / "windows-x86_64"
            linux.mkdir()
            win.mkdir()
            for name in ("fleet-host", "fleet-driver"):
                (linux / name).write_bytes(name.encode())
            for name in ("fleet-host.exe", "fleet-driver.exe"):
                (win / name).write_bytes(name.encode())
            env = {**os.environ, "FLEET_GIT_SHA": "test-tree-id"}
            env.pop("FLEET_MANIFEST_STAMP", None)
            r = subprocess.run(
                [
                    "bash",
                    str(REPO / "scripts" / "stage-worker-dist.sh"),
                    "--write-manifest",
                    str(root),
                ],
                capture_output=True,
                text=True,
                env=env,
            )
            self.assertEqual(r.returncode, 0, r.stderr + r.stdout)
            linux_man = json.loads((linux / "manifest.json").read_text())
            win_man = json.loads((win / "manifest.json").read_text())
            self.assertEqual(win_man["platform"], "windows-x86_64")
            self.assertIn("fleet-host.exe", win_man["bins"])
            self.assertIn("fleet-driver.exe", win_man["bins"])
            self.assertNotIn("fleet-host", win_man["bins"])
            self.assertNotIn("fleet-driver", win_man["bins"])
            # Unbaked refresh must not stamp FLEET_GIT_SHA (Windows keys).
            self.assertEqual(win_man["git_sha"], "")
            self.assertEqual(
                win_man["bins"]["fleet-host.exe"]["size"],
                len(b"fleet-host.exe"),
            )
            self.assertEqual(
                linux_man["bins"]["fleet-host"]["version"],
                win_man["bins"]["fleet-host.exe"]["version"],
                "package version is the same on every OS",
            )
            self.assertEqual(
                linux_man["bins"]["fleet-driver"]["version"],
                win_man["bins"]["fleet-driver.exe"]["version"],
                "package version is the same on every OS",
            )

    def test_stage_write_manifest_stamp_success(self):
        """FLEET_MANIFEST_STAMP=1 records FLEET_GIT_SHA on unbaked bins."""
        import json

        with tempfile.TemporaryDirectory() as td:
            linux = Path(td) / "linux-x86_64"
            linux.mkdir()
            (linux / "fleet-host").write_bytes(b"fleet-host")
            (linux / "fleet-driver").write_bytes(b"fleet-driver")
            r = subprocess.run(
                [
                    "bash",
                    str(REPO / "scripts" / "stage-worker-dist.sh"),
                    "--write-manifest",
                    str(linux),
                ],
                capture_output=True,
                text=True,
                env={
                    **os.environ,
                    "FLEET_GIT_SHA": "test-tree-id",
                    "FLEET_MANIFEST_STAMP": "1",
                },
            )
            self.assertEqual(r.returncode, 0, r.stderr + r.stdout)
            man = json.loads((linux / "manifest.json").read_text())
            self.assertEqual(man["git_sha"], "test-tree-id")

    def test_stage_write_manifest_uses_bake_embedded_in_host(self):
        """git_sha is the 40-hex bake inside fleet-host, not FLEET_GIT_SHA."""
        import json

        baked = "a" * 40
        with tempfile.TemporaryDirectory() as td:
            linux = Path(td) / "linux-x86_64"
            linux.mkdir()
            (linux / "fleet-host").write_bytes(f"{baked}git_sha".encode())
            (linux / "fleet-driver").write_bytes(b"fleet-driver")
            env = {**os.environ, "FLEET_GIT_SHA": "other-tree-id"}
            env.pop("FLEET_MANIFEST_STAMP", None)
            r = subprocess.run(
                [
                    "bash",
                    str(REPO / "scripts" / "stage-worker-dist.sh"),
                    "--write-manifest",
                    str(linux),
                ],
                capture_output=True,
                text=True,
                env=env,
            )
            self.assertEqual(r.returncode, 0, r.stderr + r.stdout)
            man = json.loads((linux / "manifest.json").read_text())
            self.assertEqual(man["git_sha"], baked)
            self.assertNotEqual(man["git_sha"], "")
            self.assertNotEqual(man["git_sha"], "other-tree-id")

    def test_stage_write_manifest_versions_match_runner_packages_toml(self):
        """bins.*.version equal the [versions] literals in runner-packages.toml."""
        import json
        import re

        toml = (REPO / "runner-packages.toml").read_text()
        versions_m = re.search(r"(?ms)^\[versions\]\s*\n(.*?)(?=^\[|\Z)", toml)
        self.assertIsNotNone(versions_m, "runner-packages.toml [versions] table")
        versions = versions_m.group(1)
        host_m = re.search(r'(?m)^fleet-host\s*=\s*"([^"]+)"', versions)
        driver_m = re.search(r'(?m)^fleet-driver\s*=\s*"([^"]+)"', versions)
        self.assertIsNotNone(host_m, "runner-packages.toml [versions] fleet-host")
        self.assertIsNotNone(driver_m, "runner-packages.toml [versions] fleet-driver")
        host_ver = host_m.group(1)
        driver_ver = driver_m.group(1)

        with tempfile.TemporaryDirectory() as td:
            linux = Path(td) / "linux-x86_64"
            linux.mkdir()
            (linux / "fleet-host").write_bytes(b"fleet-host")
            (linux / "fleet-driver").write_bytes(b"fleet-driver")
            env = {**os.environ, "FLEET_GIT_SHA": "test-tree-id"}
            env.pop("FLEET_MANIFEST_STAMP", None)
            r = subprocess.run(
                [
                    "bash",
                    str(REPO / "scripts" / "stage-worker-dist.sh"),
                    "--write-manifest",
                    str(linux),
                ],
                capture_output=True,
                text=True,
                env=env,
            )
            self.assertEqual(r.returncode, 0, r.stderr + r.stdout)
            man = json.loads((linux / "manifest.json").read_text())
            self.assertEqual(man["bins"]["fleet-host"]["version"], host_ver)
            self.assertEqual(man["bins"]["fleet-driver"]["version"], driver_ver)

    def test_stage_manifest_missing_platform_or_hash_not_current(self):
        """Empty dest, host-only, driver-only, or 64-hex bake id leave no manifest.json."""
        import hashlib

        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            empty = subprocess.run(
                [
                    "bash",
                    str(REPO / "scripts" / "stage-worker-dist.sh"),
                    "--write-manifest",
                    str(root),
                ],
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(empty.returncode, 0, empty.stdout + empty.stderr)
            self.assertFalse((root / "manifest.json").exists())

            host_only = root / "linux-x86_64"
            host_only.mkdir()
            (host_only / "fleet-host").write_bytes(b"fleet-host")
            missing_driver = subprocess.run(
                [
                    "bash",
                    str(REPO / "scripts" / "stage-worker-dist.sh"),
                    "--write-manifest",
                    str(root),
                ],
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(
                missing_driver.returncode, 0, missing_driver.stdout + missing_driver.stderr
            )
            self.assertFalse((host_only / "manifest.json").exists())

            (host_only / "fleet-host").unlink()
            (host_only / "fleet-driver").write_bytes(b"fleet-driver")
            missing_host = subprocess.run(
                [
                    "bash",
                    str(REPO / "scripts" / "stage-worker-dist.sh"),
                    "--write-manifest",
                    str(root),
                ],
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(
                missing_host.returncode, 0, missing_host.stdout + missing_host.stderr
            )
            self.assertFalse((host_only / "manifest.json").exists())

            (host_only / "fleet-host").write_bytes(b"fleet-host")
            file_hash = hashlib.sha256(b"fleet-host").hexdigest()
            stamped_hash = subprocess.run(
                [
                    "bash",
                    str(REPO / "scripts" / "stage-worker-dist.sh"),
                    "--write-manifest",
                    str(host_only),
                ],
                capture_output=True,
                text=True,
                env={
                    **os.environ,
                    "FLEET_GIT_SHA": file_hash,
                    "FLEET_MANIFEST_STAMP": "1",
                },
            )
            self.assertNotEqual(
                stamped_hash.returncode,
                0,
                stamped_hash.stdout + stamped_hash.stderr,
            )
            self.assertFalse((host_only / "manifest.json").exists())

    def test_stage_write_manifest_windows_exe_missing_driver_not_current(self):
        """windows-x86_64 with only fleet-host.exe must write no manifest.json."""
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            win = root / "windows-x86_64"
            win.mkdir()
            (win / "fleet-host.exe").write_bytes(b"fleet-host.exe")
            r = subprocess.run(
                [
                    "bash",
                    str(REPO / "scripts" / "stage-worker-dist.sh"),
                    "--write-manifest",
                    str(root),
                ],
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertFalse((win / "manifest.json").exists())
            self.assertFalse((root / "manifest.json").exists())

    def test_install_skips_download_when_sha256_matches(self):
        """Matching dest + CP sidecar must not GET the bin."""
        import hashlib
        import threading
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

        common = (SCRIPT_DIR / "_unix_common.sh").read_bytes()
        plat = (SCRIPT_DIR / "linux-x86_64.sh").read_bytes()
        bins = {
            "fleet-host": b"#!/bin/sh\n# host\nexit 0\n",
            "fleet-driver": b"#!/bin/sh\n# driver\nexit 0\n",
        }
        sidecars = {
            name: (hashlib.sha256(body).hexdigest() + "\n").encode()
            for name, body in bins.items()
        }
        gets: list[str] = []

        class Handler(BaseHTTPRequestHandler):
            def _body(self):
                path = self.path.split("?", 1)[0]
                mapping = {
                    "/health": b'{"status":"ok"}\n',
                    "/install/worker/_unix_common.sh": common,
                    "/install/worker/linux-x86_64.sh": plat,
                    "/install/worker/bin/linux-x86_64/fleet-host": bins["fleet-host"],
                    "/install/worker/bin/linux-x86_64/fleet-driver": bins["fleet-driver"],
                    "/install/worker/bin/linux-x86_64/fleet-host.sha256": sidecars[
                        "fleet-host"
                    ],
                    "/install/worker/bin/linux-x86_64/fleet-driver.sha256": sidecars[
                        "fleet-driver"
                    ],
                }
                return mapping.get(path)

            def do_HEAD(self):
                body = self._body()
                if body is None:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()

            def do_GET(self):
                path = self.path.split("?", 1)[0]
                gets.append(path)
                body = self._body()
                if body is None:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, fmt, *args):
                return

        httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        port = httpd.server_address[1]
        thread = threading.Thread(target=httpd.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as td:
                td = Path(td)
                prefix = td / "prefix"
                bindir = prefix / "bin"
                bindir.mkdir(parents=True)
                for name, body in bins.items():
                    dest = bindir / name
                    dest.write_bytes(body)
                    dest.chmod(0o755)
                env = os.environ.copy()
                env["FLEET_URL"] = f"http://127.0.0.1:{port}"
                env["FLEET_PLATFORM"] = "linux-x86_64"
                env["FLEET_PREFIX"] = str(prefix)
                env["HOME"] = str(td)
                env.pop("FLEET_DETECT_ONLY", None)
                env["FLEET_PAIR_TOKEN"] = "flp_testdeadbeef"
                r = subprocess.run(
                    ["sh", str(SCRIPT_DIR / "linux-x86_64.sh")],
                    capture_output=True,
                    text=True,
                    env=env,
                )
                out = (r.stdout or "") + (r.stderr or "")
                self.assertEqual(r.returncode, 0, out)
                self.assertEqual((bindir / "fleet-host").read_bytes(), bins["fleet-host"])
                self.assertEqual(
                    (bindir / "fleet-driver").read_bytes(), bins["fleet-driver"]
                )
                self.assertIn(
                    "/install/worker/bin/linux-x86_64/fleet-host.sha256", gets
                )
                self.assertIn(
                    "/install/worker/bin/linux-x86_64/fleet-driver.sha256", gets
                )
                self.assertNotIn("/install/worker/bin/linux-x86_64/fleet-host", gets)
                self.assertNotIn("/install/worker/bin/linux-x86_64/fleet-driver", gets)
        finally:
            httpd.shutdown()
            httpd.server_close()

    def test_install_refetches_when_local_sha256_stale(self):
        """Stale dest + sidecar of the new bin must GET the bin and replace dest."""
        import hashlib
        import threading
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

        common = (SCRIPT_DIR / "_unix_common.sh").read_bytes()
        plat = (SCRIPT_DIR / "linux-x86_64.sh").read_bytes()
        stale = {
            "fleet-host": b"stale-host\n",
            "fleet-driver": b"stale-driver\n",
        }
        bins = {
            "fleet-host": b"#!/bin/sh\n# host\nexit 0\n",
            "fleet-driver": b"#!/bin/sh\n# driver\nexit 0\n",
        }
        sidecars = {
            name: (hashlib.sha256(body).hexdigest() + "\n").encode()
            for name, body in bins.items()
        }
        gets: list[str] = []

        class Handler(BaseHTTPRequestHandler):
            def _body(self):
                path = self.path.split("?", 1)[0]
                mapping = {
                    "/health": b'{"status":"ok"}\n',
                    "/install/worker/_unix_common.sh": common,
                    "/install/worker/linux-x86_64.sh": plat,
                    "/install/worker/bin/linux-x86_64/fleet-host": bins["fleet-host"],
                    "/install/worker/bin/linux-x86_64/fleet-driver": bins["fleet-driver"],
                    "/install/worker/bin/linux-x86_64/fleet-host.sha256": sidecars[
                        "fleet-host"
                    ],
                    "/install/worker/bin/linux-x86_64/fleet-driver.sha256": sidecars[
                        "fleet-driver"
                    ],
                }
                return mapping.get(path)

            def do_HEAD(self):
                body = self._body()
                if body is None:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()

            def do_GET(self):
                path = self.path.split("?", 1)[0]
                gets.append(path)
                body = self._body()
                if body is None:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, fmt, *args):
                return

        httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        port = httpd.server_address[1]
        thread = threading.Thread(target=httpd.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as td:
                td = Path(td)
                prefix = td / "prefix"
                bindir = prefix / "bin"
                bindir.mkdir(parents=True)
                for name, body in stale.items():
                    dest = bindir / name
                    dest.write_bytes(body)
                    dest.chmod(0o755)
                env = os.environ.copy()
                env["FLEET_URL"] = f"http://127.0.0.1:{port}"
                env["FLEET_PLATFORM"] = "linux-x86_64"
                env["FLEET_PREFIX"] = str(prefix)
                env["HOME"] = str(td)
                env.pop("FLEET_DETECT_ONLY", None)
                env["FLEET_PAIR_TOKEN"] = "flp_testdeadbeef"
                r = subprocess.run(
                    ["sh", str(SCRIPT_DIR / "linux-x86_64.sh")],
                    capture_output=True,
                    text=True,
                    env=env,
                )
                out = (r.stdout or "") + (r.stderr or "")
                self.assertEqual(r.returncode, 0, out)
                self.assertEqual((bindir / "fleet-host").read_bytes(), bins["fleet-host"])
                self.assertEqual(
                    (bindir / "fleet-driver").read_bytes(), bins["fleet-driver"]
                )
                self.assertIn("/install/worker/bin/linux-x86_64/fleet-host", gets)
                self.assertIn("/install/worker/bin/linux-x86_64/fleet-driver", gets)
        finally:
            httpd.shutdown()
            httpd.server_close()

    def test_install_dies_on_checksum_mismatch_leaves_no_bad_bin(self):
        """Sidecar that does not match the served bin must leave dest unclobbered."""
        import hashlib
        import threading
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

        common = (SCRIPT_DIR / "_unix_common.sh").read_bytes()
        plat = (SCRIPT_DIR / "linux-x86_64.sh").read_bytes()
        keep = {
            "fleet-host": b"#!/bin/sh\nexit 0\n",
            "fleet-driver": b"#!/bin/sh\nexit 0\n",
        }
        garbage = {
            "fleet-host": b"GARBAGE-NOT-A-BIN\n",
            "fleet-driver": b"GARBAGE-DRIVER\n",
        }
        sidecars = {
            name: (
                hashlib.sha256(b"not-" + body).hexdigest() + "\n"
            ).encode()
            for name, body in garbage.items()
        }

        class Handler(BaseHTTPRequestHandler):
            def _body(self):
                path = self.path.split("?", 1)[0]
                mapping = {
                    "/health": b'{"status":"ok"}\n',
                    "/install/worker/_unix_common.sh": common,
                    "/install/worker/linux-x86_64.sh": plat,
                    "/install/worker/bin/linux-x86_64/fleet-host": garbage["fleet-host"],
                    "/install/worker/bin/linux-x86_64/fleet-driver": garbage[
                        "fleet-driver"
                    ],
                    "/install/worker/bin/linux-x86_64/fleet-host.sha256": sidecars[
                        "fleet-host"
                    ],
                    "/install/worker/bin/linux-x86_64/fleet-driver.sha256": sidecars[
                        "fleet-driver"
                    ],
                }
                return mapping.get(path)

            def do_HEAD(self):
                body = self._body()
                if body is None:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()

            def do_GET(self):
                body = self._body()
                if body is None:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, fmt, *args):
                return

        httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        port = httpd.server_address[1]
        thread = threading.Thread(target=httpd.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as td:
                td = Path(td)
                prefix = td / "prefix"
                bindir = prefix / "bin"
                bindir.mkdir(parents=True)
                for name, body in keep.items():
                    dest = bindir / name
                    dest.write_bytes(body)
                    dest.chmod(0o755)
                env = os.environ.copy()
                env["FLEET_URL"] = f"http://127.0.0.1:{port}"
                env["FLEET_PLATFORM"] = "linux-x86_64"
                env["FLEET_PREFIX"] = str(prefix)
                env["HOME"] = str(td)
                env.pop("FLEET_DETECT_ONLY", None)
                env["FLEET_PAIR_TOKEN"] = "flp_testdeadbeef"
                r = subprocess.run(
                    ["sh", str(SCRIPT_DIR / "linux-x86_64.sh")],
                    capture_output=True,
                    text=True,
                    env=env,
                )
                out = (r.stdout or "") + (r.stderr or "")
                self.assertNotEqual(r.returncode, 0, out)
                self.assertEqual((bindir / "fleet-host").read_bytes(), keep["fleet-host"])
                self.assertNotEqual(
                    (bindir / "fleet-host").read_bytes(), garbage["fleet-host"]
                )
                self.assertFalse((bindir / "fleet-host.tmp").exists(), out)
        finally:
            httpd.shutdown()
            httpd.server_close()

    def test_stage_copy_bins_writes_sidecar_sha256(self):
        """copy_bins writes fleet-host.sha256 whose first field is sha256 of the bin."""
        import hashlib

        stage = REPO / "scripts" / "stage-worker-dist.sh"
        prelude, sep, _ = stage.read_text().partition('case "$MODE" in')
        self.assertTrue(sep, "stage-worker-dist.sh must dispatch on MODE")
        host = b"staged-fleet-host-bytes"
        driver = b"staged-fleet-driver-bytes"
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            src = td / "src"
            out = td / "linux-x86_64"
            src.mkdir()
            (src / "fleet-host").write_bytes(host)
            (src / "fleet-driver").write_bytes(driver)
            script = (
                'COPY_BINS_SRC="$1"\n'
                'COPY_BINS_OUT="$2"\n'
                "set --\n"
                + prelude
                + f'\nROOT="{REPO}"\ncd "$ROOT"\n'
                + 'copy_bins "$COPY_BINS_SRC" "$COPY_BINS_OUT"\n'
            )
            r = subprocess.run(
                ["bash", "-c", script, "stage-copy-bins", str(src), str(out)],
                capture_output=True,
                text=True,
                env={
                    **os.environ,
                    "FLEET_GIT_SHA": "test-tree-id",
                    "FLEET_WORKER_DIST_DIR": str(td / "dist"),
                    "HOME": str(td),
                },
            )
            self.assertEqual(r.returncode, 0, r.stderr + r.stdout)
            self.assertEqual((out / "fleet-host").read_bytes(), host)
            sidecar = (out / "fleet-host.sha256").read_text().split()[0]
            self.assertEqual(sidecar, hashlib.sha256(host).hexdigest())
            driver_sidecar = (out / "fleet-driver.sha256").read_text().split()[0]
            self.assertEqual(driver_sidecar, hashlib.sha256(driver).hexdigest())

    def test_windows_get_pemachine_runs_on_cached_dest_and_download_tmp_before_host_ready(self):
        """Get-PeMachine must inspect cached dest and download tmp before hostReady=true."""
        for name in ("windows-x86_64.ps1", "windows-arm64.ps1"):
            text = (SCRIPT_DIR / name).read_text(encoding="ascii")
            dest_at = text.find("Get-PeMachine $dest")
            tmp_at = text.find("Get-PeMachine $tmp")
            last_ready = text.rfind("$hostReady = $true")
            self.assertNotEqual(dest_at, -1, f"{name} must PE-check cached dest")
            self.assertNotEqual(tmp_at, -1, f"{name} must PE-check download tmp")
            self.assertNotEqual(last_ready, -1, f"{name} must set hostReady after a valid PE")
            self.assertLess(dest_at, last_ready, name)
            self.assertLess(tmp_at, last_ready, name)
            fn_at = text.find("function Get-PeMachine")
            self.assertNotEqual(fn_at, -1, name)
            pe_fn = text[fn_at : text.find("function ", fn_at + 1)]
            self.assertIn("0x8664", pe_fn, name)
            self.assertIn("0xAA64", pe_fn, name)

    def test_windows_host_ready_starts_false_and_only_sets_on_valid_fleet_host_exe(self):
        """hostReady is false until a WantPe fleet-host.exe; driver or leftover exe is not enough."""
        gated = 'if ($bin -eq "fleet-host.exe") { $hostReady = $true }'
        for name in ("windows-x86_64.ps1", "windows-arm64.ps1"):
            text = (SCRIPT_DIR / name).read_text(encoding="ascii")
            ready_false = text.find("$hostReady = $false")
            foreach_at = text.find("foreach ($bin in")
            self.assertNotEqual(ready_false, -1, name)
            self.assertNotEqual(foreach_at, -1, name)
            self.assertLess(ready_false, foreach_at, name)
            self.assertIn(gated, text, name)
            self.assertNotIn("$hostReady = $true", text.replace(gated, ""), name)
            first_pe = text.find("if ($pe -ne $WantPe)")
            first_true = text.find("$hostReady = $true")
            second_pe = text.find("if ($pe -ne $WantPe)", first_pe + 1)
            second_true = text.find("$hostReady = $true", first_true + 1)
            self.assertNotEqual(first_pe, -1, name)
            self.assertNotEqual(second_pe, -1, f"{name} must WantPe-check dest and tmp")
            self.assertNotEqual(second_true, -1, f"{name} must set hostReady on cache hit and download")
            self.assertLess(first_pe, first_true, name)
            self.assertLess(second_pe, second_true, name)

    def test_windows_wrong_arch_or_invalid_host_throw_before_pair(self):
        """Refuse the other machine arch and an invalid host before pairing a leftover exe."""
        for name in ("windows-x86_64.ps1", "windows-arm64.ps1"):
            text = (SCRIPT_DIR / name).read_text(encoding="ascii")
            pair_at = text.find("pair --url")
            start_at = text.find("Start-Process")
            self.assertNotEqual(pair_at, -1, name)
            self.assertNotEqual(start_at, -1, name)
            refuse_if = text.find("if ($detected -ne $ThisPlat)")
            self.assertNotEqual(refuse_if, -1, name)
            refuse_end = text.find("}", refuse_if)
            self.assertIn("throw", text[refuse_if:refuse_end], name)
            self.assertLess(refuse_if, pair_at, name)
            self.assertLess(refuse_if, start_at, name)
            gate = text.find("if (-not $hostReady)")
            self.assertNotEqual(gate, -1, name)
            gate_end = text.find("}", gate)
            self.assertIn("throw", text[gate:gate_end], name)
            self.assertLess(gate, pair_at, name)
            self.assertLess(gate, start_at, name)

    def _run_linux_already_paired_install(
        self,
        *,
        leftover_token: str = "flm_x",
        leftover_host: bytes | None = None,
        serve_bins: bool = True,
    ):
        """Exec linux-x86_64.sh with leftover machine.token and no FLEET_PAIR_TOKEN."""
        import threading
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

        common = (SCRIPT_DIR / "_unix_common.sh").read_bytes()
        plat = (SCRIPT_DIR / "linux-x86_64.sh").read_bytes()

        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            events_log = td / "events.log"
            events_log.write_text("")
            host_log = td / "host.invocations"
            prefix = td / "prefix"
            bindir = prefix / "bin"
            bindir.mkdir(parents=True)
            if leftover_host is not None:
                dest = bindir / "fleet-host"
                dest.write_bytes(leftover_host)
                dest.chmod(0o755)

            host_bin = (
                "#!/bin/sh\n"
                f"printf '%s\\n' \"$*\" >> '{host_log}'\n"
                "exit 0\n"
            ).encode()
            bins = {
                "fleet-host": host_bin,
                "fleet-driver": b"#!/bin/sh\nexit 0\n",
            }

            spy = td / "spy"
            spy.mkdir()
            (spy / "systemctl").write_text(
                "#!/bin/sh\n"
                f"printf 'systemctl %s\\n' \"$*\" >> '{events_log}'\n"
                "exit 0\n"
            )
            (spy / "systemctl").chmod(0o755)

            cfg = td / "cfg"
            cfg.mkdir()
            (cfg / "machine.token").write_text(leftover_token)

            class Handler(BaseHTTPRequestHandler):
                def do_GET(self):
                    path = self.path.split("?", 1)[0]
                    if path.startswith("/install/worker/bin/"):
                        with events_log.open("a", encoding="utf-8") as fh:
                            fh.write(f"GET {path}\n")
                    mapping = {
                        "/health": b'{"status":"ok"}\n',
                        "/install/worker/_unix_common.sh": common,
                        "/install/worker/linux-x86_64.sh": plat,
                    }
                    if serve_bins:
                        mapping[
                            "/install/worker/bin/linux-x86_64/fleet-host"
                        ] = bins["fleet-host"]
                        mapping[
                            "/install/worker/bin/linux-x86_64/fleet-driver"
                        ] = bins["fleet-driver"]
                    body = mapping.get(path)
                    if body is None:
                        self.send_error(404)
                        return
                    self.send_response(200)
                    self.send_header("Content-Type", "application/octet-stream")
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)

                def log_message(self, fmt, *args):
                    return

            httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
            port = httpd.server_address[1]
            thread = threading.Thread(target=httpd.serve_forever, daemon=True)
            thread.start()
            try:
                env = os.environ.copy()
                env["PATH"] = f"{spy}{os.pathsep}{env.get('PATH', '')}"
                env["FLEET_URL"] = f"http://127.0.0.1:{port}"
                env["FLEET_PLATFORM"] = "linux-x86_64"
                env["FLEET_PREFIX"] = str(prefix)
                env["HOME"] = str(td)
                env["FLEET_HOST_CONFIG_DIR"] = str(cfg)
                env.pop("FLEET_DETECT_ONLY", None)
                env.pop("FLEET_PAIR_TOKEN", None)
                env.pop("FLEET_WORKER_BUILD_FROM_SOURCE", None)
                env.pop("FLEET_INSTALL_ORIGIN", None)
                env.pop("FLEET_REPO", None)
                r = subprocess.run(
                    ["sh", str(SCRIPT_DIR / "linux-x86_64.sh")],
                    capture_output=True,
                    text=True,
                    env=env,
                )
                dest = bindir / "fleet-host"
                dest_bytes = dest.read_bytes() if dest.is_file() else None
                host_inv = (
                    host_log.read_text(encoding="utf-8")
                    if host_log.exists()
                    else ""
                )
                events = events_log.read_text(encoding="utf-8")
                return r, dest_bytes, host_bin, events, host_inv
            finally:
                httpd.shutdown()
                httpd.server_close()

    def test_reinstall_restarts_already_paired_host(self):
        """Leftover non-empty machine.token without FLEET_PAIR_TOKEN installs and starts."""
        old = b"old-fleet-host\n"
        r, dest_bytes, host_bin, events, host_inv = (
            self._run_linux_already_paired_install(
                leftover_token="flm_x",
                leftover_host=old,
                serve_bins=True,
            )
        )
        out = (r.stdout or "") + (r.stderr or "")
        self.assertEqual(r.returncode, 0, out)
        self.assertNotIn("FLEET_PAIR_TOKEN is not set", out)
        self.assertEqual(dest_bytes, host_bin)
        self.assertNotIn("pair", host_inv)
        self.assertIn("stop fleet-host", events, out + "\n" + events)
        self.assertIn("restart fleet-host", events, out + "\n" + events)
        self.assertLess(
            events.find("stop fleet-host"),
            events.find("GET /install/worker/bin/"),
            out + "\n" + events,
        )
        self.assertLess(
            events.find("GET /install/worker/bin/"),
            events.find("restart fleet-host"),
            out + "\n" + events,
        )

    def test_already_paired_bin_fail_restarts_previous_host(self):
        """Already-paired reinstall with no CP bins must restart the previous host then die."""
        old = b"previous-fleet-host\n"
        r, dest_bytes, _host_bin, events, host_inv = (
            self._run_linux_already_paired_install(
                leftover_token="flm_x",
                leftover_host=old,
                serve_bins=False,
            )
        )
        out = (r.stdout or "") + (r.stderr or "")
        self.assertNotEqual(r.returncode, 0, out)
        self.assertNotIn("FLEET_PAIR_TOKEN is not set", out)
        self.assertEqual(dest_bytes, old)
        self.assertNotIn("pair", host_inv)
        self.assertIn("stop fleet-host", events, out + "\n" + events)
        self.assertIn("restart fleet-host", events, out + "\n" + events)
        self.assertLess(
            events.find("stop fleet-host"),
            events.find("restart fleet-host"),
            out + "\n" + events,
        )
        self.assertIn("GET /install/worker/bin/linux-x86_64/fleet-host", events)

    def test_windows_already_paired_start_process_run(self):
        """windows-*.ps1 already-paired branch starts fleet-host.exe run; stop before download."""
        import re

        for name in ("windows-x86_64.ps1", "windows-arm64.ps1"):
            text = (SCRIPT_DIR / name).read_text(encoding="ascii")
            stop_at = text.find("Stop-Process")
            loop_at = text.find("foreach ($bin in")
            self.assertNotEqual(stop_at, -1, name)
            self.assertNotEqual(loop_at, -1, name)
            self.assertLess(
                stop_at,
                loop_at,
                f"{name} must stop fleet-host before replacing the PE",
            )
            m = re.search(
                r"elseif\s*\(\s*\$alreadyPaired\s*\)\s*\{(.*?)\n\}",
                text,
                re.S,
            )
            self.assertIsNotNone(m, f"{name} missing elseif ($alreadyPaired)")
            self.assertRegex(
                m.group(1),
                r"Start-Process[\s\S]*fleet-host\.exe[\s\S]*\brun\b",
                f"{name} elseif ($alreadyPaired) must Start-Process fleet-host.exe run",
            )

    def _extract_sh_fn(self, src: str, name: str) -> str:
        import re

        start = src.find(f"{name}() {{")
        self.assertNotEqual(start, -1, f"missing {name}()")
        m = re.search(r"\n}\s*\n", src[start:])
        self.assertIsNotNone(m, f"unclosed {name}()")
        return src[start : start + m.end() - 1]

    def _unix_pair_arm_src(self, common: str) -> str:
        marker = '"$BIN_DIR/fleet-host" pair'
        at = common.find(marker)
        self.assertNotEqual(at, -1, "missing fleet-host pair")
        head = common.rfind('if [ -n "${FLEET_PAIR_TOKEN:-}" ]; then', 0, at)
        self.assertNotEqual(head, -1, "pair is not under FLEET_PAIR_TOKEN")
        tail = common.find("\nelif ", at)
        if tail < 0:
            tail = common.find("\nelse", at)
        self.assertNotEqual(tail, -1, "unclosed FLEET_PAIR_TOKEN pair arm")
        return common[head:tail] + "\nfi\n"

    def _run_unix_common_pair(
        self,
        *,
        platform: str,
        spies: tuple[str, ...],
        existing_url: str | None = None,
        pair_force: bool = False,
    ):
        """Exec _unix_common.sh main with FLEET_PAIR_TOKEN.

        existing_url seeds a leftover pair (machine.token + config.toml).
        """
        import threading
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            events_log = td / "events.log"
            events_log.write_text("")
            prefix = td / "prefix"
            (prefix / "bin").mkdir(parents=True)
            cfg = td / "cfg"
            cfg.mkdir()
            if existing_url:
                (cfg / "machine.token").write_text("flm_existing\n")
                (cfg / "config.toml").write_text(
                    f'control_plane_url = "{existing_url}"\n'
                )
            spy = td / "spy"
            spy.mkdir()
            for name in spies:
                (spy / name).write_text(
                    "#!/bin/sh\n"
                    f"printf '{name} %s\\n' \"$*\" >> '{events_log}'\n"
                    "exit 0\n"
                )
                (spy / name).chmod(0o755)

            host_bin = (
                "#!/bin/sh\n"
                f"printf 'fleet-host %s\\n' \"$*\" >> '{events_log}'\n"
                "exit 0\n"
            ).encode()
            driver_bin = b"#!/bin/sh\nexit 0\n"

            class Handler(BaseHTTPRequestHandler):
                def do_GET(self):
                    path = self.path.split("?", 1)[0]
                    mapping = {
                        "/health": b'{"status":"ok"}\n',
                        f"/install/worker/bin/{platform}/fleet-host": host_bin,
                        f"/install/worker/bin/{platform}/fleet-driver": driver_bin,
                    }
                    body = mapping.get(path)
                    if body is None:
                        self.send_error(404)
                        return
                    self.send_response(200)
                    self.send_header("Content-Type", "application/octet-stream")
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)

                def log_message(self, fmt, *args):
                    return

            httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
            port = httpd.server_address[1]
            thread = threading.Thread(target=httpd.serve_forever, daemon=True)
            thread.start()
            try:
                env = os.environ.copy()
                env["PATH"] = f"{spy}{os.pathsep}{env.get('PATH', '')}"
                env["FLEET_URL"] = f"http://127.0.0.1:{port}"
                env["FLEET_PLATFORM"] = platform
                env["FLEET_PREFIX"] = str(prefix)
                env["HOME"] = str(td)
                env["FLEET_HOST_CONFIG_DIR"] = str(cfg)
                env["FLEET_PAIR_TOKEN"] = "flp_testdeadbeef"
                if pair_force:
                    env["FLEET_PAIR_FORCE"] = "1"
                else:
                    env.pop("FLEET_PAIR_FORCE", None)
                env.pop("FLEET_DETECT_ONLY", None)
                env.pop("FLEET_WORKER_BUILD_FROM_SOURCE", None)
                env.pop("FLEET_INSTALL_ORIGIN", None)
                env.pop("FLEET_REPO", None)
                r = subprocess.run(
                    ["sh", str(SCRIPT_DIR / "_unix_common.sh")],
                    capture_output=True,
                    text=True,
                    env=env,
                )
                return r, events_log.read_text(encoding="utf-8")
            finally:
                httpd.shutdown()
                httpd.server_close()

    def test_unix_pair_arm_invokes_start_host_service_after_fleet_host_pair(self):
        """FLEET_PAIR_TOKEN main must start after pair; already_paired/bin-fail start is not enough."""
        cases = (
            ("linux-x86_64", ("systemctl",), "restart fleet-host"),
            ("darwin-arm64", ("launchctl",), None),
            ("freebsd-amd64", (), None),
        )
        for platform, spies, linux_start in cases:
            with self.subTest(platform=platform):
                if platform.startswith("freebsd"):
                    r, events = self._exec_extracted_unix_pair_arm(platform)
                else:
                    r, events = self._run_unix_common_pair(
                        platform=platform, spies=spies
                    )
                out = (r.stdout or "") + (r.stderr or "")
                self.assertEqual(r.returncode, 0, out + "\n" + events)
                self.assertIn("fleet-host pair", events, out + "\n" + events)
                if linux_start is not None:
                    start_at = events.find(linux_start)
                elif platform.startswith("darwin"):
                    start_at = events.find("launchctl bootstrap")
                    if start_at < 0:
                        start_at = events.find("launchctl load")
                else:
                    start_at = events.find("rc.d restart")
                    if start_at < 0:
                        start_at = events.find("rc.d start")
                pair_at = events.find("fleet-host pair")
                self.assertNotEqual(
                    start_at,
                    -1,
                    f"{platform} pair arm must start the supervisor after pair\n"
                    + out
                    + "\n"
                    + events,
                )
                self.assertLess(pair_at, start_at, events)

    def _exec_extracted_unix_pair_arm(self, platform: str):
        """Pair arm + start_host_service with stub supervisors (FreeBSD rc.d is not root-writable)."""
        common = (SCRIPT_DIR / "_unix_common.sh").read_text()
        start_fn = self._extract_sh_fn(common, "start_host_service")
        pair_arm = self._unix_pair_arm_src(common)
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            events = td / "events.log"
            events.write_text("")
            bindir = td / "bin"
            bindir.mkdir()
            (bindir / "fleet-host").write_text(
                "#!/bin/sh\n"
                f"printf 'fleet-host %s\\n' \"$*\" >> '{events}'\n"
                "exit 0\n"
            )
            (bindir / "fleet-host").chmod(0o755)
            spy = td / "spy"
            spy.mkdir()
            rcd = td / "rc.d-fleet-host"
            rcd.write_text(
                "#!/bin/sh\n"
                f"printf 'rc.d %s\\n' \"$*\" >> '{events}'\n"
                "exit 0\n"
            )
            rcd.chmod(0o755)
            (spy / "id").write_text("#!/bin/sh\necho 0\n")
            (spy / "id").chmod(0o755)
            (spy / "sysrc").write_text(
                "#!/bin/sh\n"
                f"printf 'sysrc %s\\n' \"$*\" >> '{events}'\n"
                "exit 0\n"
            )
            (spy / "sysrc").chmod(0o755)
            remapped = start_fn.replace("/usr/local/etc/rc.d/fleet-host", str(rcd))
            self.assertNotEqual(
                remapped,
                start_fn,
                "start_host_service must invoke /usr/local/etc/rc.d/fleet-host",
            )
            helpers = (
                self._extract_sh_fn(common, "norm_pair_url")
                + "\n"
                + self._extract_sh_fn(common, "paired_control_plane_url")
                + "\n"
                + self._extract_sh_fn(common, "confirm_unpair_and_pair")
                + "\n"
            )
            script = (
                "set -eu\n"
                'info() { printf "%s\\n" "$*"; }\n'
                'die() { printf "%s\\n" "$*" >&2; exit 1; }\n'
                'have() { command -v "$1" >/dev/null 2>&1; }\n'
                f'PLATFORM={platform}\n'
                f'BIN_DIR="{bindir}"\n'
                f'HOME="{td}"\n'
                f'ORIGIN=http://127.0.0.1:9\n'
                f"FLEET_PAIR_TOKEN=flp_testdeadbeef\n"
                "already_paired=0\n"
                + helpers
                + remapped
                + "\n"
                + pair_arm
                + "\n"
            )
            r = subprocess.run(
                ["sh", "-c", script],
                capture_output=True,
                text=True,
                env={
                    **os.environ,
                    "PATH": f"{spy}{os.pathsep}{os.environ.get('PATH', '')}",
                    "HOME": str(td),
                },
            )
            return r, events.read_text(encoding="utf-8")

    def test_start_host_service_darwin_freebsd_executes_supervisor_not_hint(self):
        """darwin/freebsd start_host_service must invoke launchctl or rc.d, not only print a hint."""
        common = (SCRIPT_DIR / "_unix_common.sh").read_text()
        start_fn = self._extract_sh_fn(common, "start_host_service")
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            events = td / "events.log"
            events.write_text("")
            bindir = td / "bin"
            bindir.mkdir()
            spy = td / "spy"
            spy.mkdir()
            (spy / "launchctl").write_text(
                "#!/bin/sh\n"
                f"printf 'launchctl %s\\n' \"$*\" >> '{events}'\n"
                "exit 0\n"
            )
            (spy / "launchctl").chmod(0o755)
            home = td / "home"
            plist = home / "Library" / "LaunchAgents" / "ai.agentfleet.host.plist"
            plist.parent.mkdir(parents=True)
            plist.write_text("plist\n")
            script = (
                "set -eu\n"
                'info() { printf "%s\\n" "$*"; }\n'
                'have() { command -v "$1" >/dev/null 2>&1; }\n'
                "PLATFORM=darwin-arm64\n"
                f'BIN_DIR="{bindir}"\n'
                f'HOME="{home}"\n'
                + start_fn
                + "\nstart_host_service\n"
            )
            r = subprocess.run(
                ["sh", "-c", script],
                capture_output=True,
                text=True,
                env={
                    **os.environ,
                    "PATH": f"{spy}{os.pathsep}{os.environ.get('PATH', '')}",
                    "HOME": str(home),
                },
            )
            out = (r.stdout or "") + (r.stderr or "")
            log = events.read_text(encoding="utf-8")
            self.assertEqual(r.returncode, 0, out + "\n" + log)
            self.assertTrue(
                "launchctl bootstrap" in log or "launchctl load" in log,
                out + "\n" + log,
            )

        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            events = td / "events.log"
            events.write_text("")
            bindir = td / "bin"
            bindir.mkdir()
            spy = td / "spy"
            spy.mkdir()
            rcd = td / "rc.d-fleet-host"
            rcd.write_text(
                "#!/bin/sh\n"
                f"printf 'rc.d %s\\n' \"$*\" >> '{events}'\n"
                "exit 0\n"
            )
            rcd.chmod(0o755)
            (spy / "id").write_text("#!/bin/sh\necho 0\n")
            (spy / "id").chmod(0o755)
            (spy / "sysrc").write_text(
                "#!/bin/sh\n"
                f"printf 'sysrc %s\\n' \"$*\" >> '{events}'\n"
                "exit 0\n"
            )
            (spy / "sysrc").chmod(0o755)
            remapped = start_fn.replace("/usr/local/etc/rc.d/fleet-host", str(rcd))
            self.assertNotEqual(
                remapped,
                start_fn,
                "start_host_service must invoke /usr/local/etc/rc.d/fleet-host",
            )
            script = (
                "set -eu\n"
                'info() { printf "%s\\n" "$*"; }\n'
                'have() { command -v "$1" >/dev/null 2>&1; }\n'
                "PLATFORM=freebsd-amd64\n"
                f'BIN_DIR="{bindir}"\n'
                f'HOME="{td}"\n'
                + remapped
                + "\nstart_host_service\n"
            )
            r = subprocess.run(
                ["sh", "-c", script],
                capture_output=True,
                text=True,
                env={
                    **os.environ,
                    "PATH": f"{spy}{os.pathsep}{os.environ.get('PATH', '')}",
                    "HOME": str(td),
                },
            )
            out = (r.stdout or "") + (r.stderr or "")
            log = events.read_text(encoding="utf-8")
            self.assertEqual(r.returncode, 0, out + "\n" + log)
            self.assertTrue(
                "rc.d restart" in log or "rc.d start" in log,
                out + "\n" + log,
            )

    def test_windows_pair_token_branch_start_process_run(self):
        """windows-*.ps1 if ($pairToken) must Start-Process fleet-host.exe run after pair."""
        import re

        for name in ("windows-x86_64.ps1", "windows-arm64.ps1"):
            text = (SCRIPT_DIR / name).read_text(encoding="ascii")
            m = re.search(
                r"if\s*\(\s*\$pairToken\s*\)\s*\{([^}]*pair --url[^}]*)\}",
                text,
                re.S,
            )
            self.assertIsNotNone(
                m, f"{name} missing if ($pairToken) branch that runs pair --url"
            )
            self.assertRegex(
                m.group(1),
                r"Start-Process[\s\S]*fleet-host\.exe[\s\S]*\brun\b",
                f"{name} if ($pairToken) must Start-Process fleet-host.exe run",
            )

    def test_fetch_only_path_installs_freebsd_bins(self):
        """No curl/wget: fetch -o dest url must place host and driver (dest survives)."""
        import shutil
        import tempfile
        import threading
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

        common = (SCRIPT_DIR / "_unix_common.sh").read_bytes()
        plat = (SCRIPT_DIR / "freebsd-amd64.sh").read_bytes()
        bins = {
            "fleet-host": b"#!/bin/sh\nexit 0\n",
            "fleet-driver": b"#!/bin/sh\nexit 0\n",
        }

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                mapping = {
                    "/health": b'{"status":"ok"}\n',
                    "/install/worker/_unix_common.sh": common,
                    "/install/worker/freebsd-amd64.sh": plat,
                    "/install/worker/bin/freebsd-amd64/fleet-host": bins["fleet-host"],
                    "/install/worker/bin/freebsd-amd64/fleet-driver": bins["fleet-driver"],
                }
                body = mapping.get(self.path)
                if body is None:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, fmt, *args):
                return

        httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        port = httpd.server_address[1]
        thread = threading.Thread(target=httpd.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as td:
                td = Path(td)
                bindir = td / "bin"
                bindir.mkdir()
                fetch_log = td / "fetch.log"
                fetch = bindir / "fetch"
                fetch.write_text(
                    "#!/bin/sh\n"
                    # fetch -o dest url  (dest=- → stdout)
                    f'echo "$@" >> "{fetch_log}"\n'
                    'dest=""; url=""\n'
                    'while [ $# -gt 0 ]; do\n'
                    '  case $1 in\n'
                    '    -o) dest=$2; shift 2 ;;\n'
                    '    -q|-qo) if [ "$1" = "-qo" ]; then dest=$2; shift 2; else shift; fi ;;\n'
                    '    -*) shift ;;\n'
                    '    *) url=$1; shift ;;\n'
                    '  esac\n'
                    'done\n'
                    '[ -n "$dest" ] && [ -n "$url" ] || exit 2\n'
                    'if [ "$dest" = "-" ]; then\n'
                    '  python3 -c "import sys,urllib.request; sys.stdout.buffer.write(urllib.request.urlopen(sys.argv[1]).read())" "$url"\n'
                    'else\n'
                    '  python3 -c "import sys,urllib.request; urllib.request.urlretrieve(sys.argv[1], sys.argv[2])" "$url" "$dest"\n'
                    'fi\n'
                )
                fetch.chmod(0o755)
                needed = (
                    "sh",
                    "mkdir",
                    "chmod",
                    "mv",
                    "rm",
                    "id",
                    "printf",
                    "echo",
                    "grep",
                    "head",
                    "cat",
                    "uname",
                    "mktemp",
                    "sleep",
                    "sed",
                    "tr",
                    "cut",
                    "wc",
                    "ls",
                    "test",
                    "true",
                    "false",
                    "env",
                    "date",
                    "python3",
                    "basename",
                    "dirname",
                    "touch",
                    "[",
                )
                for name in needed:
                    src = shutil.which(name)
                    if src:
                        dest = bindir / name
                        if not dest.exists():
                            dest.symlink_to(src)
                prefix = td / "prefix"
                env = os.environ.copy()
                env["PATH"] = str(bindir)
                env["FLEET_URL"] = f"http://127.0.0.1:{port}"
                env["FLEET_PLATFORM"] = "freebsd-amd64"
                env["FLEET_PREFIX"] = str(prefix)
                env["HOME"] = str(td)
                env.pop("FLEET_DETECT_ONLY", None)
                env["FLEET_PAIR_TOKEN"] = "flp_testdeadbeef"
                # Confirm curl/wget are hidden.
                hidden = subprocess.run(
                    ["sh", "-c", "command -v curl; command -v wget; command -v fetch"],
                    capture_output=True,
                    text=True,
                    env=env,
                )
                self.assertNotIn("curl", hidden.stdout)
                self.assertNotIn("wget", hidden.stdout)
                self.assertIn("fetch", hidden.stdout)

                r = subprocess.run(
                    ["sh", str(SCRIPT_DIR / "freebsd-amd64.sh")],
                    capture_output=True,
                    text=True,
                    env=env,
                )
                out = (r.stdout or "") + (r.stderr or "")
                self.assertEqual(r.returncode, 0, out)
                bindest = prefix / "bin"
                host = bindest / "fleet-host"
                driver = bindest / "fleet-driver"
                self.assertTrue(host.is_file(), out)
                self.assertTrue(driver.is_file(), out)
                self.assertEqual(host.read_bytes(), bins["fleet-host"])
                self.assertEqual(driver.read_bytes(), bins["fleet-driver"])
                self.assertFalse((bindest / "fleet-host.tmp").exists(), out)
                self.assertFalse((bindest / "fleet-driver.tmp").exists(), out)
                log = fetch_log.read_text()
                self.assertIn("-o", log)
                # dest-leak: fetch wrote to *.tmp then mv, never dest=
                self.assertNotIn("dest=", log)
        finally:
            httpd.shutdown()
            httpd.server_close()

    def test_install_worker_sh_fetch_only_http_get_downloads_freebsd_wrapper(self):
        """No curl/wget: install-worker.sh http_get must fetch -o - freebsd-amd64.sh."""
        import shutil
        import tempfile
        import threading
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

        wrapper = b"#!/bin/sh\nexit 0\n"
        gets: list[str] = []

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                path = self.path.split("?", 1)[0]
                gets.append(path)
                mapping = {
                    "/health": b'{"status":"ok"}\n',
                    "/install/worker/freebsd-amd64.sh": wrapper,
                }
                body = mapping.get(path)
                if body is None:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, fmt, *args):
                return

        httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        port = httpd.server_address[1]
        thread = threading.Thread(target=httpd.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as td:
                td = Path(td)
                bindir = td / "bin"
                bindir.mkdir()
                fetch_log = td / "fetch.log"
                fetch = bindir / "fetch"
                fetch.write_text(
                    "#!/bin/sh\n"
                    f'echo "$@" >> "{fetch_log}"\n'
                    'dest=""; url=""\n'
                    'while [ $# -gt 0 ]; do\n'
                    '  case $1 in\n'
                    '    -o) dest=$2; shift 2 ;;\n'
                    '    -q|-qo) if [ "$1" = "-qo" ]; then dest=$2; shift 2; else shift; fi ;;\n'
                    '    -*) shift ;;\n'
                    '    *) url=$1; shift ;;\n'
                    '  esac\n'
                    'done\n'
                    '[ -n "$dest" ] && [ -n "$url" ] || exit 2\n'
                    'if [ "$dest" = "-" ]; then\n'
                    '  python3 -c "import sys,urllib.request; sys.stdout.buffer.write(urllib.request.urlopen(sys.argv[1]).read())" "$url"\n'
                    'else\n'
                    '  python3 -c "import sys,urllib.request; urllib.request.urlretrieve(sys.argv[1], sys.argv[2])" "$url" "$dest"\n'
                    'fi\n'
                )
                fetch.chmod(0o755)
                _write_fake_uname_ldd(bindir, "FreeBSD", "amd64")
                needed = (
                    "sh",
                    "mkdir",
                    "chmod",
                    "mv",
                    "rm",
                    "id",
                    "printf",
                    "echo",
                    "grep",
                    "head",
                    "cat",
                    "mktemp",
                    "sed",
                    "tr",
                    "cut",
                    "python3",
                    "basename",
                    "dirname",
                    "[",
                )
                for name in needed:
                    src = shutil.which(name)
                    if src:
                        dest = bindir / name
                        if not dest.exists():
                            dest.symlink_to(src)
                env = os.environ.copy()
                env["PATH"] = str(bindir)
                env["FLEET_URL"] = f"http://127.0.0.1:{port}"
                env["FLEET_PLATFORM"] = "freebsd-amd64"
                env["FLEET_PREFIX"] = str(td / "prefix")
                env["HOME"] = str(td)
                env.pop("FLEET_DETECT_ONLY", None)
                env["FLEET_PAIR_TOKEN"] = "flp_testdeadbeef"
                hidden = subprocess.run(
                    ["sh", "-c", "command -v curl; command -v wget; command -v fetch"],
                    capture_output=True,
                    text=True,
                    env=env,
                )
                self.assertNotIn("curl", hidden.stdout)
                self.assertNotIn("wget", hidden.stdout)
                self.assertIn("fetch", hidden.stdout)

                r = subprocess.run(
                    ["sh", str(BOOTSTRAP)],
                    capture_output=True,
                    text=True,
                    env=env,
                )
                out = (r.stdout or "") + (r.stderr or "")
                self.assertEqual(r.returncode, 0, out)
                self.assertIn("/install/worker/freebsd-amd64.sh", gets)
                log = fetch_log.read_text()
                self.assertIn(
                    f"-o - http://127.0.0.1:{port}/install/worker/freebsd-amd64.sh",
                    log,
                )
        finally:
            httpd.shutdown()
            httpd.server_close()

    def test_linux_curl_path_writes_host_and_driver_with_progress_bar(self):
        """Hide wget/fetch: curl --progress-bar must write served host and driver bytes."""
        import shutil
        import tempfile
        import threading
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

        common = (SCRIPT_DIR / "_unix_common.sh").read_bytes()
        plat = (SCRIPT_DIR / "linux-x86_64.sh").read_bytes()
        bins = {
            "fleet-host": b"#!/bin/sh\n# curl-path-host\nexit 0\n",
            "fleet-driver": b"#!/bin/sh\n# curl-path-driver\nexit 0\n",
        }
        real_curl = shutil.which("curl")
        self.assertIsNotNone(real_curl, "curl must be on PATH")

        class Handler(BaseHTTPRequestHandler):
            def _body(self):
                mapping = {
                    "/health": b'{"status":"ok"}\n',
                    "/install/worker/_unix_common.sh": common,
                    "/install/worker/linux-x86_64.sh": plat,
                    "/install/worker/bin/linux-x86_64/fleet-host": bins["fleet-host"],
                    "/install/worker/bin/linux-x86_64/fleet-driver": bins["fleet-driver"],
                }
                return mapping.get(self.path.split("?", 1)[0])

            def do_HEAD(self):
                body = self._body()
                if body is None:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()

            def do_GET(self):
                body = self._body()
                if body is None:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, fmt, *args):
                return

        httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        port = httpd.server_address[1]
        thread = threading.Thread(target=httpd.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as td:
                td = Path(td)
                bindir = td / "bin"
                bindir.mkdir()
                curl_log = td / "curl.log"
                (bindir / "curl").write_text(
                    "#!/bin/sh\n"
                    f'echo "$@" >> "{curl_log}"\n'
                    f'exec "{real_curl}" "$@"\n'
                )
                (bindir / "curl").chmod(0o755)
                needed = (
                    "sh",
                    "mkdir",
                    "chmod",
                    "mv",
                    "rm",
                    "id",
                    "printf",
                    "echo",
                    "grep",
                    "head",
                    "cat",
                    "uname",
                    "mktemp",
                    "sleep",
                    "sed",
                    "tr",
                    "cut",
                    "awk",
                    "wc",
                    "ls",
                    "test",
                    "true",
                    "false",
                    "env",
                    "date",
                    "basename",
                    "dirname",
                    "touch",
                    "[",
                )
                for name in needed:
                    src = shutil.which(name)
                    if src:
                        dest = bindir / name
                        if not dest.exists():
                            dest.symlink_to(src)
                prefix = td / "prefix"
                env = os.environ.copy()
                env["PATH"] = str(bindir)
                env["FLEET_URL"] = f"http://127.0.0.1:{port}"
                env["FLEET_PLATFORM"] = "linux-x86_64"
                env["FLEET_PREFIX"] = str(prefix)
                env["HOME"] = str(td)
                env.pop("FLEET_DETECT_ONLY", None)
                env["FLEET_PAIR_TOKEN"] = "flp_testdeadbeef"
                hidden = subprocess.run(
                    ["sh", "-c", "command -v curl; command -v wget; command -v fetch"],
                    capture_output=True,
                    text=True,
                    env=env,
                )
                self.assertIn("curl", hidden.stdout)
                self.assertNotIn("wget", hidden.stdout)
                self.assertNotIn("fetch", hidden.stdout)

                r = subprocess.run(
                    ["sh", str(SCRIPT_DIR / "linux-x86_64.sh")],
                    capture_output=True,
                    text=True,
                    env=env,
                )
                out = (r.stdout or "") + (r.stderr or "")
                self.assertEqual(r.returncode, 0, out)
                bindest = prefix / "bin"
                host = bindest / "fleet-host"
                driver = bindest / "fleet-driver"
                self.assertTrue(host.is_file(), out)
                self.assertTrue(driver.is_file(), out)
                self.assertEqual(host.read_bytes(), bins["fleet-host"])
                self.assertEqual(driver.read_bytes(), bins["fleet-driver"])
                log = curl_log.read_text()
                origin = f"http://127.0.0.1:{port}"
                self.assertIn(f"-fsSL {origin}/install/worker/_unix_common.sh", log)
                self.assertIn(
                    f"-fL --progress-bar {origin}/install/worker/bin/linux-x86_64/fleet-host",
                    log,
                )
                self.assertIn(
                    f"-fL --progress-bar {origin}/install/worker/bin/linux-x86_64/fleet-driver",
                    log,
                )
        finally:
            httpd.shutdown()
            httpd.server_close()

    def test_linux_wget_only_wrapper_and_fetch_to_installs_host(self):
        """Hide curl: wget -qO- common and wget -O dest must write fleet-host."""
        import shutil
        import tempfile
        import threading
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

        common = (SCRIPT_DIR / "_unix_common.sh").read_bytes()
        plat = (SCRIPT_DIR / "linux-x86_64.sh").read_bytes()
        bins = {
            "fleet-host": b"#!/bin/sh\n# wget-path-host\nexit 0\n",
            "fleet-driver": b"#!/bin/sh\n# wget-path-driver\nexit 0\n",
        }

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                mapping = {
                    "/health": b'{"status":"ok"}\n',
                    "/install/worker/_unix_common.sh": common,
                    "/install/worker/linux-x86_64.sh": plat,
                    "/install/worker/bin/linux-x86_64/fleet-host": bins["fleet-host"],
                    "/install/worker/bin/linux-x86_64/fleet-driver": bins["fleet-driver"],
                }
                body = mapping.get(self.path.split("?", 1)[0])
                if body is None:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, fmt, *args):
                return

        httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        port = httpd.server_address[1]
        thread = threading.Thread(target=httpd.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as td:
                td = Path(td)
                bindir = td / "bin"
                bindir.mkdir()
                wget_log = td / "wget.log"
                (bindir / "wget").write_text(
                    "#!/bin/sh\n"
                    f'echo "$@" >> "{wget_log}"\n'
                    'dest=""; url=""\n'
                    "while [ $# -gt 0 ]; do\n"
                    "  case $1 in\n"
                    '    -qO-) dest=-; shift ;;\n'
                    '    -qO|-O) dest=$2; shift 2 ;;\n'
                    "    -q) shift ;;\n"
                    "    -*) shift ;;\n"
                    "    *) url=$1; shift ;;\n"
                    "  esac\n"
                    "done\n"
                    '[ -n "$dest" ] && [ -n "$url" ] || exit 2\n'
                    'if [ "$dest" = "-" ]; then\n'
                    '  python3 -c "import sys,urllib.request; sys.stdout.buffer.write(urllib.request.urlopen(sys.argv[1]).read())" "$url" || exit 1\n'
                    "else\n"
                    '  python3 -c "import sys,urllib.request; urllib.request.urlretrieve(sys.argv[1], sys.argv[2])" "$url" "$dest" || exit 1\n'
                    "fi\n"
                )
                (bindir / "wget").chmod(0o755)
                needed = (
                    "sh",
                    "mkdir",
                    "chmod",
                    "mv",
                    "rm",
                    "id",
                    "printf",
                    "echo",
                    "grep",
                    "head",
                    "cat",
                    "uname",
                    "mktemp",
                    "sleep",
                    "sed",
                    "tr",
                    "cut",
                    "awk",
                    "python3",
                    "basename",
                    "dirname",
                    "touch",
                    "[",
                )
                for name in needed:
                    src = shutil.which(name)
                    if src:
                        dest = bindir / name
                        if not dest.exists():
                            dest.symlink_to(src)
                prefix = td / "prefix"
                env = os.environ.copy()
                env["PATH"] = str(bindir)
                env["FLEET_URL"] = f"http://127.0.0.1:{port}"
                env["FLEET_PLATFORM"] = "linux-x86_64"
                env["FLEET_PREFIX"] = str(prefix)
                env["HOME"] = str(td)
                env.pop("FLEET_DETECT_ONLY", None)
                env["FLEET_PAIR_TOKEN"] = "flp_testdeadbeef"
                hidden = subprocess.run(
                    ["sh", "-c", "command -v curl; command -v wget; command -v fetch"],
                    capture_output=True,
                    text=True,
                    env=env,
                )
                self.assertNotIn("curl", hidden.stdout)
                self.assertIn("wget", hidden.stdout)
                self.assertNotIn("fetch", hidden.stdout)

                r = subprocess.run(
                    ["sh", str(SCRIPT_DIR / "linux-x86_64.sh")],
                    capture_output=True,
                    text=True,
                    env=env,
                )
                out = (r.stdout or "") + (r.stderr or "")
                self.assertEqual(r.returncode, 0, out)
                host = prefix / "bin" / "fleet-host"
                self.assertTrue(host.is_file(), out)
                self.assertEqual(host.read_bytes(), bins["fleet-host"])
                log = wget_log.read_text()
                origin = f"http://127.0.0.1:{port}"
                self.assertIn(f"-qO- {origin}/install/worker/_unix_common.sh", log)
                self.assertIn(
                    f"-O {prefix}/bin/fleet-host.tmp {origin}/install/worker/bin/linux-x86_64/fleet-host",
                    log,
                )
        finally:
            httpd.shutdown()
            httpd.server_close()

    def test_url_host_strips_userinfo(self):
        """url_host drops userinfo at the last @."""
        cases = {
            "http://127.0.0.1:8080": "127.0.0.1",
            "http://user@127.0.0.1:8080": "127.0.0.1",
            "http://user:pass@127.0.0.1:8080": "127.0.0.1",
            "http://a@b:c@127.0.0.1:8080": "127.0.0.1",
            "https://user@localhost/path": "localhost",
            "http://user@[::1]:8080": "::1",
            "http://[::1]:8080": "::1",
        }
        for name in ("_unix_common.sh", "install-worker.sh"):
            fn = self._extract_sh_fn((SCRIPT_DIR / name).read_text(), "url_host")
            for url, want in cases.items():
                r = subprocess.run(
                    ["sh", "-c", fn + '\nurl_host "$1"', "url_host", url],
                    capture_output=True,
                    text=True,
                )
                self.assertEqual(r.returncode, 0, f"{name} {url}: {r.stderr}")
                self.assertEqual(r.stdout.strip(), want, f"{name} {url}")

    def test_loopback_userinfo_url_dies_before_mkdir_or_fetch(self):
        """user:pass@127.0.0.1 FLEET_URL must refuse before mkdir or bin fetch."""
        import threading
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

        common = (SCRIPT_DIR / "_unix_common.sh").read_bytes()
        plat = (SCRIPT_DIR / "linux-x86_64.sh").read_bytes()
        hits: list[str] = []

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                path = self.path.split("?", 1)[0]
                hits.append(path)
                mapping = {
                    "/install/worker/_unix_common.sh": common,
                    "/install/worker/linux-x86_64.sh": plat,
                }
                body = mapping.get(path)
                if body is None:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, fmt, *args):
                return

        httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        port = httpd.server_address[1]
        thread = threading.Thread(target=httpd.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as td:
                td = Path(td)
                prefix = td / "prefix"
                env = os.environ.copy()
                env["FLEET_URL"] = f"http://user:pass@127.0.0.1:{port}"
                env["FLEET_PLATFORM"] = "linux-x86_64"
                env["FLEET_PREFIX"] = str(prefix)
                env["HOME"] = str(td)
                env.pop("FLEET_DETECT_ONLY", None)
                env["FLEET_PAIR_TOKEN"] = "flp_testdeadbeef"
                r = subprocess.run(
                    ["sh", str(SCRIPT_DIR / "linux-x86_64.sh")],
                    capture_output=True,
                    text=True,
                    env=env,
                )
                out = (r.stdout or "") + (r.stderr or "")
                self.assertNotEqual(r.returncode, 0, out)
                self.assertFalse(prefix.exists(), out)
                self.assertFalse((prefix / "bin" / "fleet-host").exists(), out)
                self.assertFalse(
                    any("/install/worker/bin/" in p for p in hits),
                    hits,
                )
                self.assertIn("/health", hits)
        finally:
            httpd.shutdown()
            httpd.server_close()

    def test_loopback_missing_health_dies_before_mkdir_or_fetch(self):
        """Loopback origin without GET /health must die before mkdir or bin fetch."""
        import tempfile
        import threading
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

        common = (SCRIPT_DIR / "_unix_common.sh").read_bytes()
        plat = (SCRIPT_DIR / "linux-x86_64.sh").read_bytes()
        hits: list[str] = []

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                path = self.path.split("?", 1)[0]
                hits.append(path)
                mapping = {
                    "/install/worker/_unix_common.sh": common,
                    "/install/worker/linux-x86_64.sh": plat,
                }
                body = mapping.get(path)
                if body is None:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, fmt, *args):
                return

        httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        port = httpd.server_address[1]
        thread = threading.Thread(target=httpd.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as td:
                td = Path(td)
                prefix = td / "prefix"
                env = os.environ.copy()
                env["FLEET_URL"] = f"http://127.0.0.1:{port}"
                env["FLEET_PLATFORM"] = "linux-x86_64"
                env["FLEET_PREFIX"] = str(prefix)
                env["HOME"] = str(td)
                env.pop("FLEET_DETECT_ONLY", None)
                env["FLEET_PAIR_TOKEN"] = "flp_testdeadbeef"
                r = subprocess.run(
                    ["sh", str(SCRIPT_DIR / "linux-x86_64.sh")],
                    capture_output=True,
                    text=True,
                    env=env,
                )
                out = (r.stdout or "") + (r.stderr or "")
                self.assertNotEqual(r.returncode, 0, out)
                self.assertFalse(prefix.exists(), out)
                self.assertFalse((prefix / "bin" / "fleet-host").exists(), out)
                self.assertFalse(
                    any("/install/worker/bin/" in p for p in hits),
                    hits,
                )
                self.assertIn("/health", hits)
        finally:
            httpd.shutdown()
            httpd.server_close()

    def test_norm_pair_url_trims_slash_and_space(self):
        fn = self._extract_sh_fn(
            (SCRIPT_DIR / "_unix_common.sh").read_text(), "norm_pair_url"
        )
        cases = {
            "https://agentfleet.studio": "https://agentfleet.studio",
            "https://agentfleet.studio/": "https://agentfleet.studio",
            "  http://192.168.1.10:18280/  ": "http://192.168.1.10:18280",
        }
        for raw, want in cases.items():
            r = subprocess.run(
                ["sh", "-c", fn + '\nnorm_pair_url "$1"', "norm", raw],
                capture_output=True,
                text=True,
            )
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(r.stdout, want, raw)

    def test_paired_control_plane_url_reads_toml(self):
        fn = self._extract_sh_fn(
            (SCRIPT_DIR / "_unix_common.sh").read_text(), "paired_control_plane_url"
        )
        with tempfile.TemporaryDirectory() as td:
            cfg = Path(td)
            (cfg / "config.toml").write_text(
                'run_host = true\ncontrol_plane_url = "http://192.168.1.10:18280/"\n'
            )
            r = subprocess.run(
                ["sh", "-c", fn + "\npaired_control_plane_url\n"],
                capture_output=True,
                text=True,
                env={**os.environ, "FLEET_HOST_CONFIG_DIR": str(cfg)},
            )
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(r.stdout.strip(), "http://192.168.1.10:18280/")

    def test_confirm_unpair_force_env_skips_prompt(self):
        src = (SCRIPT_DIR / "_unix_common.sh").read_text()
        die = "die() { printf '%s\\n' \"$*\" >&2; exit 1; }\n"
        info = "info() { :; }\n"
        fn = self._extract_sh_fn(src, "confirm_unpair_and_pair")
        r = subprocess.run(
            [
                "sh",
                "-c",
                die + info + fn + '\nconfirm_unpair_and_pair "$1" "$2"\n',
                "confirm",
                "http://old",
                "https://new",
            ],
            capture_output=True,
            text=True,
            env={**os.environ, "FLEET_PAIR_FORCE": "1"},
            stdin=subprocess.DEVNULL,
        )
        self.assertEqual(r.returncode, 0, r.stderr + r.stdout)

    def test_confirm_unpair_nontty_tells_how_to_force(self):
        src = (SCRIPT_DIR / "_unix_common.sh").read_text()
        die = "die() { printf '%s\\n' \"$*\" >&2; exit 1; }\n"
        info = "info() { :; }\n"
        fn = self._extract_sh_fn(src, "confirm_unpair_and_pair")
        r = subprocess.run(
            [
                "sh",
                "-c",
                die + info + fn + '\nconfirm_unpair_and_pair "$1" "$2"\n',
                "confirm",
                "http://old",
                "https://new",
            ],
            capture_output=True,
            text=True,
            env={**os.environ, "FLEET_PAIR_FORCE": ""},
            stdin=subprocess.DEVNULL,
        )
        err = (r.stderr or "") + (r.stdout or "")
        self.assertNotEqual(r.returncode, 0, err)
        self.assertIn("already paired to", err)
        self.assertIn("FLEET_PAIR_FORCE=1", err)

    def test_unix_url_switch_force_passes_force_to_fleet_host(self):
        """Existing pair to another URL + FLEET_PAIR_FORCE=1 must call pair --force."""
        r, events = self._run_unix_common_pair(
            platform="linux-x86_64",
            spies=("systemctl",),
            existing_url="http://192.168.1.10:18280",
            pair_force=True,
        )
        out = (r.stdout or "") + (r.stderr or "")
        self.assertEqual(r.returncode, 0, out + "\n" + events)
        self.assertIn("fleet-host pair", events)
        self.assertIn("--force", events, events)

    def test_unix_url_switch_without_force_or_tty_keeps_old_pair(self):
        r, events = self._run_unix_common_pair(
            platform="linux-x86_64",
            spies=("systemctl",),
            existing_url="http://192.168.1.10:18280",
            pair_force=False,
        )
        out = (r.stdout or "") + (r.stderr or "")
        self.assertNotEqual(r.returncode, 0, out + "\n" + events)
        self.assertIn("already paired to http://192.168.1.10:18280", out)
        self.assertNotIn("fleet-host pair", events)

    def test_all_installers_prompt_on_url_switch(self):
        unix = (SCRIPT_DIR / "_unix_common.sh").read_text()
        self.assertIn("confirm_unpair_and_pair", unix)
        self.assertIn("FLEET_PAIR_FORCE", unix)
        self.assertIn("This machine is already paired to", unix)
        for name in ("windows-x86_64.ps1", "windows-arm64.ps1"):
            raw = (SCRIPT_DIR / name).read_text()
            self.assertIn("Confirm-UnpairAndPair", raw, name)
            self.assertIn("FLEET_PAIR_FORCE", raw, name)
            self.assertIn("This machine is already paired to", raw, name)
            self.assertIn("--force", raw, name)


class VerifyInstallTests(unittest.TestCase):
    """Read-only verdicts: deleted install vs disabled vs stopped vs running."""

    def _run(self, home: Path, bindir: Path) -> subprocess.CompletedProcess[str]:
        env = {
            "HOME": str(home),
            "PATH": f"{bindir}:/usr/bin:/bin",
            "FLEET_HOST_CONFIG_DIR": str(home / ".config" / "fleet-host"),
        }
        return subprocess.run(
            ["sh", str(SCRIPT_DIR / "verify.sh")],
            capture_output=True,
            text=True,
            env=env,
        )

    def _fakes(self, bindir: Path, *, enabled: str, active: str, process: bool) -> None:
        bindir.mkdir(parents=True)
        (bindir / "systemctl").write_text(
            "#!/bin/sh\n"
            "cmd=\n"
            "for a in \"$@\"; do\n"
            "  case \"$a\" in\n"
            "    is-enabled|is-active) cmd=$a ;;\n"
            "  esac\n"
            "done\n"
            f"if [ \"$cmd\" = is-enabled ]; then echo {enabled}; "
            f"[ {enabled} = enabled ]; exit; fi\n"
            f"if [ \"$cmd\" = is-active ]; then echo {active}; "
            f"[ {active} = active ]; exit; fi\n"
            "echo unexpected >&2\n"
            "exit 99\n",
            encoding="utf-8",
        )
        (bindir / "pgrep").write_text(
            "#!/bin/sh\n" + ("exit 0\n" if process else "exit 1\n"),
            encoding="utf-8",
        )
        os.chmod(bindir / "systemctl", 0o755)
        os.chmod(bindir / "pgrep", 0o755)

    def _verdict(self, proc: subprocess.CompletedProcess[str]) -> str:
        for line in proc.stdout.splitlines():
            if line.startswith("verdict:"):
                return line.split(":", 1)[1].strip()
        self.fail(proc.stdout + proc.stderr)

    def test_empty_home_is_not_installed(self):
        with tempfile.TemporaryDirectory() as td:
            home = Path(td)
            bindir = home / "fakebin"
            self._fakes(bindir, enabled="disabled", active="inactive", process=False)
            proc = self._run(home, bindir)
            self.assertEqual(proc.returncode, 4, proc.stdout + proc.stderr)
            self.assertEqual(self._verdict(proc), "not_installed")
            self.assertNotIn("flm_", proc.stdout)

    def test_config_without_binary_is_bins_missing_and_hides_token(self):
        with tempfile.TemporaryDirectory() as td:
            home = Path(td)
            cfg = home / ".config" / "fleet-host"
            cfg.mkdir(parents=True)
            (cfg / "config.toml").write_text("run_host = true\n", encoding="utf-8")
            (cfg / "machine.token").write_text("flm_secretvalue\n", encoding="utf-8")
            bindir = home / "fakebin"
            self._fakes(bindir, enabled="disabled", active="inactive", process=False)
            proc = self._run(home, bindir)
            self.assertEqual(proc.returncode, 4, proc.stdout + proc.stderr)
            self.assertEqual(self._verdict(proc), "bins_missing")
            self.assertIn("token:           present", proc.stdout)
            self.assertNotIn("flm_secretvalue", proc.stdout)

    def test_disabled_unit_is_service_disabled(self):
        with tempfile.TemporaryDirectory() as td:
            home = Path(td)
            bin_path = home / ".local" / "bin" / "fleet-host"
            bin_path.parent.mkdir(parents=True)
            bin_path.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            os.chmod(bin_path, 0o755)
            unit = home / ".config" / "systemd" / "user" / "fleet-host.service"
            unit.parent.mkdir(parents=True)
            unit.write_text("[Service]\nExecStart=/bin/true\n", encoding="utf-8")
            bindir = home / "fakebin"
            self._fakes(bindir, enabled="disabled", active="inactive", process=False)
            proc = self._run(home, bindir)
            self.assertEqual(proc.returncode, 2, proc.stdout + proc.stderr)
            self.assertEqual(self._verdict(proc), "service_disabled")
            self.assertIn("systemctl --user enable --now fleet-host", proc.stdout)

    def test_enabled_inactive_is_service_stopped(self):
        with tempfile.TemporaryDirectory() as td:
            home = Path(td)
            bin_path = home / ".local" / "bin" / "fleet-host"
            bin_path.parent.mkdir(parents=True)
            bin_path.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            os.chmod(bin_path, 0o755)
            unit = home / ".config" / "systemd" / "user" / "fleet-host.service"
            unit.parent.mkdir(parents=True)
            unit.write_text("[Service]\nExecStart=/bin/true\n", encoding="utf-8")
            bindir = home / "fakebin"
            self._fakes(bindir, enabled="enabled", active="inactive", process=False)
            proc = self._run(home, bindir)
            self.assertEqual(proc.returncode, 3, proc.stdout + proc.stderr)
            self.assertEqual(self._verdict(proc), "service_stopped")
            self.assertIn("systemctl --user start fleet-host", proc.stdout)

    def test_running_process_wins_over_disabled_unit(self):
        with tempfile.TemporaryDirectory() as td:
            home = Path(td)
            bin_path = home / ".local" / "bin" / "fleet-host"
            bin_path.parent.mkdir(parents=True)
            bin_path.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            os.chmod(bin_path, 0o755)
            unit = home / ".config" / "systemd" / "user" / "fleet-host.service"
            unit.parent.mkdir(parents=True)
            unit.write_text("[Service]\nExecStart=/bin/true\n", encoding="utf-8")
            bindir = home / "fakebin"
            self._fakes(bindir, enabled="disabled", active="inactive", process=True)
            proc = self._run(home, bindir)
            self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
            self.assertEqual(self._verdict(proc), "service_running")


if __name__ == "__main__":
    unittest.main()

