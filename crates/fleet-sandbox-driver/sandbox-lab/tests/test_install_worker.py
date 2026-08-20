#!/usr/bin/env python3
"""Worker bootstrap: POSIX syntax + platform detect. No network."""
from __future__ import annotations

import os
import subprocess
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


class InstallWorkerTests(unittest.TestCase):
    def test_bootstrap_exists_and_executable(self):
        self.assertTrue(BOOTSTRAP.is_file(), BOOTSTRAP)
        self.assertTrue(BOOTSTRAP.stat().st_mode & 0o111, "bootstrap must be executable")

    def test_all_top10_scripts_present(self):
        for name in PLATFORMS:
            path = SCRIPT_DIR / name
            self.assertTrue(path.is_file(), path)

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
        ):
            r = subprocess.run(
                ["sh", "-n", str(SCRIPT_DIR / name)],
                capture_output=True,
                text=True,
            )
            self.assertEqual(r.returncode, 0, f"{name}: {r.stderr}")

    def test_detect_only_prints_known_id(self):
        env = os.environ.copy()
        env["FLEET_DETECT_ONLY"] = "1"
        r = subprocess.run(
            ["sh", str(BOOTSTRAP)],
            capture_output=True,
            text=True,
            env=env,
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        plat = r.stdout.strip()
        known = {
            "linux-x86_64",
            "linux-aarch64",
            "linux-armv7",
            "linux-x86_64-musl",
            "linux-aarch64-musl",
            "linux-armv7-musl",
            "darwin-arm64",
            "darwin-x86_64",
            "freebsd-amd64",
            "windows-x86_64",
            "windows-arm64",
        }
        self.assertIn(plat, known, plat)

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

    def test_piped_without_url_fails_closed(self):
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

    def test_windows_arm64_fetches_libunwind_optional(self):
        """gnullvm PE imports libunwind.dll; 404 must not fail the install."""
        arm = (SCRIPT_DIR / "windows-arm64.ps1").read_text()
        boot = (SCRIPT_DIR / "install-worker.ps1").read_text()
        x64 = (SCRIPT_DIR / "windows-x86_64.ps1").read_text()
        self.assertIn("libunwind.dll", arm)
        self.assertIn("no runtime", arm)
        self.assertIn("libunwind.dll", boot)
        self.assertIn("no runtime libunwind.dll", boot)
        self.assertNotIn("libunwind.dll", x64)

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

    def test_scripts_use_fetch_and_keep_dest_leak_fix(self):
        """Stock FreeBSD has fetch(1), not curl/wget. dest= clobbers caller dest."""
        common = (SCRIPT_DIR / "_unix_common.sh").read_text()
        boot = BOOTSTRAP.read_text()
        self.assertIn("_dest=$2", common)
        self.assertNotRegex(common, r"(?m)^\s*dest=\$2\s*$")
        self.assertIn('fetch -o "$_dest" "$_url"', common)
        self.assertNotIn('need curl or wget', common)
        self.assertIn("need curl, wget, or fetch", common)
        self.assertIn("fetch -o -", boot)
        self.assertIn("need curl, wget, or fetch", boot)
        self.assertNotIn("need curl or wget to download", boot)
        unix_plats = [n for n in PLATFORMS if n.endswith(".sh") and n != "_unix_common.sh"]
        for name in unix_plats:
            text = (SCRIPT_DIR / name).read_text()
            self.assertIn("_unix_common.sh", text, name)
            self.assertIn("fetch -o -", text, name)
            self.assertIn("need curl, wget, or fetch", text, name)
            self.assertNotIn("need curl or wget", text, name)
            self.assertIn("worker install: fetching installer", text, name)

    def test_install_uses_worker_product_name_and_three_bins(self):
        """Product is a Worker. fleet-host is the spawner, not a second product."""
        common = (SCRIPT_DIR / "_unix_common.sh").read_text()
        boot = BOOTSTRAP.read_text()
        self.assertIn("worker install:", common)
        self.assertNotIn("fleet-host install:", common)
        self.assertIn("worker install:", boot)
        self.assertNotIn("fleet-host install:", boot)
        self.assertIn("Install this worker from:", boot)
        self.assertNotIn("Install fleet-host from:", boot)
        self.assertIn("this machine is a self-hosted worker", common)
        self.assertIn("fleet-host   stays up", common)
        self.assertIn("fleet-worker runs each job", common)
        self.assertIn("fleet-driver", common)
        for name in PLATFORMS:
            text = (SCRIPT_DIR / name).read_text(encoding="ascii", errors="replace")
            self.assertNotIn("fleet-host install:", text, name)
            if name.endswith(".sh"):
                if "set FLEET_URL" in text and name != "install-worker.sh":
                    self.assertIn("worker install:", text, name)
        ps = (SCRIPT_DIR / "install-worker.ps1").read_text(encoding="ascii")
        self.assertIn("Install this worker from:", ps)
        self.assertNotIn("Install fleet-host from:", ps)
        for name in ("windows-x86_64.ps1", "windows-arm64.ps1"):
            text = (SCRIPT_DIR / name).read_text(encoding="ascii")
            self.assertIn("this machine is a self-hosted worker", text, name)
            self.assertIn("fleet-host   stays up", text, name)
            self.assertIn("fleet-worker runs each job", text, name)
            self.assertIn("Agent Fleet worker installer", text, name)

    def test_install_prints_progress_not_silent_downloads(self):
        """Phase lines + a progress bar so a 100MB fetch does not look frozen.

        Every OS path must participate: Unix wrappers fetch _unix_common.sh
        (which has the bar); Windows scripts have their own.
        """
        common = (SCRIPT_DIR / "_unix_common.sh").read_text()
        self.assertIn("this machine is a self-hosted worker", common)
        self.assertIn("fleet-host   stays up", common)
        self.assertIn("fleet-worker runs each job", common)
        self.assertIn("downloading $bin", common)
        self.assertIn("checking $bin", common)
        self.assertIn("size_note", common)
        self.assertIn("--progress-bar", common)
        # Bin fetch must not swallow curl progress (sha256 fetch may be quiet).
        self.assertNotRegex(
            common,
            r'fetch_to "\$base/\$bin" "\$dest\.tmp" 2>/dev/null',
        )
        boot = BOOTSTRAP.read_text()
        self.assertIn("downloading $PLATFORM installer", boot)
        ps_boot = (SCRIPT_DIR / "install-worker.ps1").read_text(encoding="ascii")
        self.assertIn("downloading $platform installer", ps_boot)
        unix_plats = [n for n in PLATFORMS if n.endswith(".sh") and n != "_unix_common.sh"]
        for name in unix_plats:
            text = (SCRIPT_DIR / name).read_text()
            self.assertIn("worker install: fetching installer", text, name)
            self.assertIn("_unix_common.sh", text, name)
        for name in ("windows-x86_64.ps1", "windows-arm64.ps1"):
            text = (SCRIPT_DIR / name).read_text(encoding="ascii")
            self.assertIn("this machine is a self-hosted worker", text, name)
            self.assertIn("downloading $bin", text, name)
            self.assertIn("checking $bin", text, name)
            self.assertIn("curl.exe", text, name)
            self.assertIn("--progress-bar", text, name)
            self.assertIn("Format-Bytes", text, name)

    def test_install_skips_download_when_sha256_matches(self):
        """Reuse a local bin only when it matches the CP checksum; else refetch."""
        common = (SCRIPT_DIR / "_unix_common.sh").read_text()
        self.assertIn("file_sha256()", common)
        self.assertIn(".sha256", common)
        self.assertIn("unchanged", common)
        self.assertIn("checksum mismatch", common)
        stage = (REPO / "scripts" / "stage-worker-dist.sh").read_text()
        docker = (REPO / "scripts" / "stage-worker-dist-docker.sh").read_text()
        self.assertIn("write_sha256()", stage)
        self.assertIn("write_sha256()", docker)
        self.assertIn(".sha256", stage)
        self.assertIn(".sha256", docker)
        for name in ("windows-x86_64.ps1", "windows-arm64.ps1"):
            text = (SCRIPT_DIR / name).read_text(encoding="ascii")
            self.assertIn("Get-FileHash", text, name)
            self.assertIn(".sha256", text, name)
            self.assertIn("unchanged", text, name)
            self.assertIn("checksum mismatch", text, name)
            self.assertIn("TimeoutSec 600", text, name)

    def test_windows_bootstrap_uses_os_architecture(self):
        """x64 PowerShell on ARM64 Windows reports PROCESSOR_ARCHITECTURE=AMD64.

        .NET Framework OSArchitecture also lies as X64 under emulation. Use the
        Machine-scoped env / HKLM value first.
        """
        for name in ("install-worker.ps1", "windows-x86_64.ps1", "windows-arm64.ps1"):
            text = (SCRIPT_DIR / name).read_text(encoding="ascii")
            self.assertIn(
                'GetEnvironmentVariable("PROCESSOR_ARCHITECTURE", "Machine")',
                text,
                name,
            )
            self.assertIn("windows-arm64", text, name)
            self.assertIn("Win32_Processor", text, name)

    def test_windows_refuses_wrong_arch_and_does_not_hide_errors(self):
        """Do not install amd64 PE on ARM64, and do not pair a leftover exe."""
        for name, want_pe, this_plat in (
            ("windows-x86_64.ps1", "amd64", "windows-x86_64"),
            ("windows-arm64.ps1", "arm64", "windows-arm64"),
        ):
            text = (SCRIPT_DIR / name).read_text(encoding="ascii")
            self.assertIn("Get-PeMachine", text, name)
            self.assertIn("0x8664", text, name)
            self.assertIn("0xAA64", text, name)
            self.assertIn(f'$WantPe = "{want_pe}"', text, name)
            self.assertIn(f'$ThisPlat = "{this_plat}"', text, name)
            self.assertIn("this machine is $detected, not $ThisPlat", text, name)
            self.assertIn("failed $bin", text, name)
            self.assertNotIn(
                'Write-Host "no prebuilt $bin at $url"',
                text,
                name,
            )
            self.assertIn("hostReady", text, name)
            self.assertIn("missing or not valid", text, name)
        boot = (SCRIPT_DIR / "install-worker.ps1").read_text(encoding="ascii")
        self.assertIn("machine arch:", boot)

    def test_every_platform_starts_daemon_after_pair(self):
        """Pair is a handshake. Every OS installer must start run after pair.

        A Linux-only fix leaves Darwin/FreeBSD/Windows offline after 30s.
        """
        common = (SCRIPT_DIR / "_unix_common.sh").read_text()
        self.assertIn("start_host_service()", common)
        self.assertIn("start_host_service || true", common)
        self.assertIn("enable --now fleet-host", common)
        self.assertIn("launchctl bootstrap", common)
        self.assertIn("launchctl load", common)
        self.assertIn("/usr/local/etc/rc.d/fleet-host start", common)
        self.assertNotIn(
            "start the daemon: $BIN_DIR/fleet-host run",
            common,
        )
        start_fn = common.split("start_host_service()", 1)[1]
        self.assertIn("linux-*", start_fn)
        self.assertIn("darwin-*", start_fn)
        self.assertIn("freebsd-*", start_fn)

        for name in ("windows-x86_64.ps1", "windows-arm64.ps1"):
            text = (SCRIPT_DIR / name).read_text(encoding="ascii")
            self.assertRegex(
                text,
                r"FLEET_PAIR_TOKEN[\s\S]*Start-Process[\s\S]*fleet-host\.exe[\s\S]*\brun\b",
                f"{name} must Start-Process fleet-host run after pair",
            )
            self.assertNotRegex(
                text,
                r'FLEET_PAIR_TOKEN[\s\S]*Write-Host "start: fleet-host run"',
                f"{name} must not leave start as a hint after pair",
            )

    def test_fetch_only_path_installs_freebsd_bins(self):
        """No curl/wget: fetch -o dest url must place fleet-host (dest-leak check)."""
        import shutil
        import socket
        import tempfile
        import threading
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

        common = (SCRIPT_DIR / "_unix_common.sh").read_bytes()
        plat = (SCRIPT_DIR / "freebsd-amd64.sh").read_bytes()
        boot = BOOTSTRAP.read_bytes()
        bins = {
            "fleet-host": b"fake-freebsd-host\n",
            "fleet-worker": b"fake-freebsd-worker\n",
            "fleet-driver": b"fake-freebsd-driver\n",
        }

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                mapping = {
                    "/install-worker.sh": boot,
                    "/install/worker/_unix_common.sh": common,
                    "/install/worker/freebsd-amd64.sh": plat,
                    "/install/worker/bin/freebsd-amd64/fleet-host": bins["fleet-host"],
                    "/install/worker/bin/freebsd-amd64/fleet-worker": bins["fleet-worker"],
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
                env.pop("FLEET_PAIR_TOKEN", None)
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
                host = prefix / "bin" / "fleet-host"
                self.assertTrue(host.is_file(), out)
                self.assertEqual(host.read_bytes(), bins["fleet-host"])
                self.assertFalse((prefix / "bin" / "fleet-host.tmp").exists(), out)
                self.assertNotIn("need curl", out)
                self.assertIn("installed", out)
                self.assertIn("this machine is a self-hosted worker", out)
                self.assertIn("fleet-host   stays up", out)
                self.assertIn("fleet-worker runs each job", out)
                self.assertIn("checking fleet-host", out)
                self.assertIn("downloading fleet-host", out)
                self.assertIn("fetching installer", out)
                log = fetch_log.read_text()
                self.assertIn("-o", log)
                # dest-leak: fetch wrote to *.tmp then mv, never dest=
                self.assertNotIn("dest=", log)
        finally:
            httpd.shutdown()
            httpd.server_close()

    def test_linux_curl_path_prints_progress_and_names_bins(self):
        """Linux (curl) must print the same worker naming + phase lines as FreeBSD."""
        import tempfile
        import threading
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

        common = (SCRIPT_DIR / "_unix_common.sh").read_bytes()
        plat = (SCRIPT_DIR / "linux-x86_64.sh").read_bytes()
        bins = {
            "fleet-host": b"fake-linux-host\n",
            "fleet-worker": b"fake-linux-worker\n",
            "fleet-driver": b"fake-linux-driver\n",
        }

        class Handler(BaseHTTPRequestHandler):
            def _body(self):
                mapping = {
                    "/install/worker/_unix_common.sh": common,
                    "/install/worker/linux-x86_64.sh": plat,
                    "/install/worker/bin/linux-x86_64/fleet-host": bins["fleet-host"],
                    "/install/worker/bin/linux-x86_64/fleet-worker": bins["fleet-worker"],
                    "/install/worker/bin/linux-x86_64/fleet-driver": bins["fleet-driver"],
                }
                return mapping.get(self.path)

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
                env = os.environ.copy()
                env["FLEET_URL"] = f"http://127.0.0.1:{port}"
                env["FLEET_PLATFORM"] = "linux-x86_64"
                env["FLEET_PREFIX"] = str(prefix)
                env["HOME"] = str(td)
                env.pop("FLEET_DETECT_ONLY", None)
                env.pop("FLEET_PAIR_TOKEN", None)
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
                self.assertIn("fetching installer", out)
                self.assertIn("this machine is a self-hosted worker", out)
                self.assertIn("fleet-host   stays up", out)
                self.assertIn("fleet-worker runs each job", out)
                self.assertIn("checking fleet-host", out)
                self.assertIn("downloading fleet-host", out)
                self.assertIn("installed", out)
        finally:
            httpd.shutdown()
            httpd.server_close()


if __name__ == "__main__":
    unittest.main()
