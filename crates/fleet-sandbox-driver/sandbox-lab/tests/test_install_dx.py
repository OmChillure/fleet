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
        self.assertRegex(text, r"(?m)^set -euo pipefail$")
        self.assertIn("install_dx()", text)
        self.assertRegex(text, r"(?m)^install_dx$")
        self.assertIn("Kind UI is not a substitute", text)
        self.assertNotRegex(text, r"WARN:.*\bdx\b")
        self.assertNotIn("Kind UI is enough", text)
        fn = text.split("install_dx()", 1)[1].split("\ninstall_dx\n", 1)[0]
        self.assertIn("exit 1", fn)
        self.assertIn("command -v dx", fn)
        self.assertNotRegex(fn, r"command -v dx[^\n]*\|\| true")
        self.assertNotIn("WARN", fn)
        after = text.split("\ninstall_dx\n", 1)[1][:800]
        self.assertIn("command -v dx", after)
        self.assertIn("exit 1", after)
        self.assertIn("dx still not on PATH after install", text)
        self.assertIn("Refusing to finish green", text)

    def test_bash_syntax(self):
        r = subprocess.run(["bash", "-n", str(SCRIPT)], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)


class InstallScriptNoCliFlagsTests(unittest.TestCase):
    """install-full-suite-deps.sh is one command; PROFILE/ENABLE_* live in .env."""

    def test_one_command_no_cli_flags_profile_enable_live_in_env(self):
        text = SCRIPT.read_text(encoding="utf-8")
        self.assertRegex(text, r"(?m)^if \[\[ \$# -gt 0 \]\]; then$")
        self.assertIn("this script takes no arguments", text)
        self.assertIn("Persist PROFILE/ENABLE_* in ${ROOT_ENV}", text)
        self.assertIn('ROOT_ENV="${REPO}/.env"', text)
        self.assertIn('. "${ROOT_ENV}"', text)
        self.assertIn('PROFILE="${PROFILE:-full}"', text)
        args_at = text.find("[[ $# -gt 0 ]]")
        sudo_at = text.find('exec sudo "${SCRIPT_PATH}"')
        source_at = text.find('. "${ROOT_ENV}"')
        packages_at = text.find("\ninstall_packages\n")
        self.assertGreater(args_at, 0)
        self.assertGreater(sudo_at, args_at, "argv reject must run before self-sudo")
        self.assertGreater(source_at, args_at, "do not source .env after a rejected CLI")
        self.assertGreater(packages_at, args_at)
        self.assertNotRegex(text, r"\bgetopts\b")
        self.assertNotRegex(text, r"\bgetopt\b")
        self.assertNotIn("--profile", text)
        self.assertNotIn("--enable-kind", text)
        self.assertNotIn("--enable-browser", text)
        self.assertNotRegex(text, r"(?m)^usage\(\)")
        head = text.split("set -euo pipefail", 1)[0]
        self.assertIn("no flags, no PROFILE= prefix", head)
        self.assertIn("Never pass them on the command line", head)
        self.assertIn("PROFILE=full", head)
        self.assertIn("PROFILE=lab", head)
        self.assertIn("PROFILE=minimal", head)
        self.assertIn("ENABLE_*", head)
        self.assertIn("repo-root .env", head)
        self.assertNotIn("WARN: dx", text)
        self.assertIn("Kind UI is not a substitute", text)

        for argv in (
            ["--help"],
            ["-h"],
            ["--profile", "full"],
            ["--profile=lab"],
            ["PROFILE=minimal"],
            ["--enable-kind=1"],
            ["--enable-browser-e2e"],
            ["full"],
            ["-p", "lab"],
        ):
            r = subprocess.run(
                ["bash", str(SCRIPT), *argv],
                capture_output=True,
                text=True,
                timeout=15,
            )
            self.assertNotEqual(r.returncode, 0, argv)
            err = ((r.stderr or "") + (r.stdout or "")).lower()
            combined = (r.stderr or "") + (r.stdout or "")
            self.assertIn("no arguments", err, argv)
            self.assertIn(".env", err, argv)
            self.assertIn("profile", err, argv)
            self.assertIn("enable_", err, argv)
            self.assertNotIn("==> loading", combined, argv)
            self.assertNotIn("==> install  user=", combined, argv)
            self.assertNotIn("==> dnf", combined, argv)
            self.assertNotIn("==> brew", combined, argv)

        agents = (REPO / "AGENTS.md").read_text(encoding="utf-8")
        agents_flat = re.sub(r"\s+", " ", agents)
        self.assertIn("Install is one command, no arguments", agents_flat)
        self.assertIn(
            "crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh",
            agents,
        )
        self.assertIn("Persist those in the", agents_flat)
        self.assertIn("repo-root `.env`", agents_flat)
        self.assertIn("Do not invent a second install guide", agents_flat)
        self.assertIn("do not pass flags on the command line", agents_flat)
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
        for rel in ("AGENTS.md", "README.md", "development.md", "docs/testing.md"):
            body = (REPO / rel).read_text(encoding="utf-8")
            self.assertIn("install-full-suite-deps.sh", body, rel)


if __name__ == "__main__":
    unittest.main(verbosity=2)
