#!/usr/bin/env python3
"""Install script contract: no args, repo-root .env, documented profiles."""
from __future__ import annotations

import subprocess
import unittest
from pathlib import Path

LAB = Path(__file__).resolve().parents[1]
REPO = LAB.parents[2]
SCRIPT = LAB / "scripts" / "install-full-suite-deps.sh"


class InstallContractTests(unittest.TestCase):
    def test_script_exists_and_executable(self):
        self.assertTrue(SCRIPT.is_file(), SCRIPT)
        self.assertTrue(SCRIPT.stat().st_mode & 0o111, "script must be executable")

    def test_bash_syntax(self):
        r = subprocess.run(["bash", "-n", str(SCRIPT)], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_rejects_arguments(self):
        r = subprocess.run(
            ["bash", str(SCRIPT), "--help"],
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(r.returncode, 0)
        err = (r.stderr or "") + (r.stdout or "")
        self.assertIn("no arguments", err.lower())
        self.assertIn(".env", err)

    def test_sources_repo_root_env(self):
        text = SCRIPT.read_text(encoding="utf-8")
        self.assertIn('ROOT_ENV="${REPO}/.env"', text)
        self.assertIn('. "${ROOT_ENV}"', text)

    def test_documents_profiles(self):
        text = SCRIPT.read_text(encoding="utf-8")
        for token in ("PROFILE=full", "PROFILE=lab", "PROFILE=minimal", "ENABLE_KIND"):
            self.assertIn(token, text, token)

    def test_no_crate_version_pin_in_script(self):
        text = SCRIPT.read_text(encoding="utf-8")
        self.assertNotIn("enum-ordinalize", text)

    def test_kubectl_kind_stage_under_repo_tmp(self):
        text = SCRIPT.read_text(encoding="utf-8")
        self.assertNotIn("/tmp/kubectl", text)
        self.assertNotIn("/tmp/kind", text)
        self.assertIn('staged="${TMP_ROOT}/${name}.$$"', text)

    def test_agents_md_points_at_script(self):
        agents = REPO / "AGENTS.md"
        self.assertTrue(agents.is_file())
        body = agents.read_text(encoding="utf-8")
        self.assertIn("crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh", body)
        self.assertIn(".env", body)
        # Keep committed notes repo-relative (no machine home / checkout path).
        self.assertNotIn("/code/", body)
        self.assertNotIn("/home/", body)


if __name__ == "__main__":
    unittest.main(verbosity=2)
