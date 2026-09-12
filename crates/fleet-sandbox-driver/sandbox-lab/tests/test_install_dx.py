#!/usr/bin/env python3
"""Host dx is required; install is one command with no CLI flags."""
from __future__ import annotations

import re
import subprocess
import unittest
from pathlib import Path

LAB = Path(__file__).resolve().parents[1]
REPO = LAB.parents[2]
SCRIPT = LAB / "scripts" / "install-full-suite-deps.sh"


class InstallDxFailClosedTests(unittest.TestCase):
    def test_set_e_install_dx_or_exit_1(self):
        text = SCRIPT.read_text(encoding="utf-8")
        # Top-level `if ! … command -v dx` then exit 1 (inverted `if` must fail).
        if_not_dx_exit = (
            r"(?m)^if ! [^\n]*command -v dx[^\n]*; then\n"
            r"(?:(?!^fi$).*\n)*?"
            r"  exit 1\n"
            r"fi$"
        )
        sites = list(re.finditer(if_not_dx_exit, text))
        self.assertGreaterEqual(
            len(sites),
            2,
            "need if-not-dx-then-exit-1 at the call and at script end",
        )
        self.assertRegex(
            text[: sites[0].start()],
            r"(?m)^install_dx\n?\Z",
            "first if-not-dx-then-exit-1 must sit at the install_dx call",
        )
        self.assertNotIn("command -v dx", text[sites[-1].end() :])
        self.assertNotRegex(sites[0].group(0), r"command -v dx[^\n]*\|\| true")
        self.assertNotRegex(sites[-1].group(0), r"command -v dx[^\n]*\|\| true")

    def test_install_dx_skips_host_cargo_install_on_openssl_4_and_uses_rust_image(self):
        text = SCRIPT.read_text(encoding="utf-8")
        host_gate = re.search(
            r"if \[\[ [^\n]*!= \*OpenSSL\\ 4\* && [^\n]*!= \*OpenSSL\\ 5\* \]\]; then\n"
            r"(.*?)\n"
            r"  else\n",
            text,
            flags=re.DOTALL,
        )
        self.assertIsNotNone(
            host_gate,
            "OpenSSL 4/5 must skip the host cargo install branch",
        )
        host_branch = host_gate.group(1)
        self.assertIn("cargo install dioxus-cli", host_branch)
        self.assertNotIn("docker run", host_branch)
        after = text[host_gate.end() :]
        run_at = after.find("docker run")
        self.assertGreaterEqual(run_at, 0, "OpenSSL 4/5 fallback must docker-run")
        docker_head = after[run_at:].split("bash -c", 1)[0]
        self.assertIn("${RUST_IMAGE}", docker_head)
        self.assertRegex(
            text,
            r'(?m)^RUST_IMAGE="\$\{RUST_IMAGE:-rust:\$\{RUST_TOOLCHAIN\}-bookworm\}"$',
        )


class InstallScriptNoCliFlagsTests(unittest.TestCase):
    """install-full-suite-deps.sh is one command; PROFILE/ENABLE_* live in .env."""

    def test_one_command_no_cli_flags_profile_enable_live_in_env(self):
        for argv in (
            ["--help"],
            ["--profile", "full"],
            ["PROFILE=minimal"],
            ["full"],
        ):
            r = subprocess.run(
                ["bash", str(SCRIPT), *argv],
                capture_output=True,
                text=True,
                timeout=15,
            )
            self.assertNotEqual(r.returncode, 0, argv)
            combined = (r.stderr or "") + (r.stdout or "")
            self.assertIn("no arguments", combined.lower(), argv)
            self.assertIn(".env", combined, argv)
            self.assertNotIn("==> loading", combined, argv)

        flag_cmd = re.compile(
            r"install-full-suite-deps\.sh\s+(?:-{1,2}[\w-]+|PROFILE=|ENABLE_)"
        )
        for rel in (
            "AGENTS.md",
            "README.md",
            "development.md",
            "docs/testing.md",
            "docs/testing-standardization.md",
        ):
            body = (REPO / rel).read_text(encoding="utf-8")
            self.assertIsNone(
                flag_cmd.search(body),
                f"{rel} must not invent a flagged install command",
            )


if __name__ == "__main__":
    unittest.main(verbosity=2)
