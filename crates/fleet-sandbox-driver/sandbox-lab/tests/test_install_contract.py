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
        for token in (
            "PROFILE=full",
            "PROFILE=lab",
            "PROFILE=minimal",
            "ENABLE_KIND",
            "ENABLE_SANDBOX_LAB",
            "ENABLE_BROWSER_E2E",
        ):
            self.assertIn(token, text, token)

    def test_no_crate_version_pin_in_script(self):
        text = SCRIPT.read_text(encoding="utf-8")
        self.assertNotIn("enum-ordinalize", text)

    def test_kubectl_kind_stage_under_repo_tmp(self):
        text = SCRIPT.read_text(encoding="utf-8")
        self.assertNotIn("/tmp/kubectl", text)
        self.assertNotIn("/tmp/kind", text)
        self.assertIn('staged="${TMP_ROOT}/${name}.$$"', text)

    def test_browser_e2e_default_full_only(self):
        text = SCRIPT.read_text(encoding="utf-8")
        self.assertRegex(
            text,
            r"full\)\s*(?:set_default ENABLE_[A-Z_]+\s+[01]\s*)*?set_default ENABLE_BROWSER_E2E 1",
            "PROFILE=full should default ENABLE_BROWSER_E2E=1",
        )
        self.assertRegex(
            text,
            r"lab\)\s*(?:set_default ENABLE_[A-Z_]+\s+[01]\s*)*?set_default ENABLE_BROWSER_E2E 0",
            "PROFILE=lab is compose-only; do not default browser e2e on",
        )
        self.assertRegex(
            text,
            r"minimal\)\s*(?:set_default ENABLE_[A-Z_]+\s+[01]\s*)*?set_default ENABLE_BROWSER_E2E 0",
            "PROFILE=minimal should default ENABLE_BROWSER_E2E=0",
        )

    def test_playwright_installs_os_deps(self):
        text = SCRIPT.read_text(encoding="utf-8")
        self.assertIn("playwright install-deps chromium", text)
        self.assertIn("chromium.launch(headless=True)", text)

    def test_full_suite_gates_browser_e2e(self):
        suite = LAB / "scripts" / "run-full-suite.sh"
        text = suite.read_text(encoding="utf-8")
        self.assertIn("tests/browser/test_app_e2e.py", text)
        self.assertIn("ENABLE_BROWSER_E2E", text)
        self.assertIn('ROOT_ENV="${REPO}/.env"', text)
        self.assertIn('. "${ROOT_ENV}"', text)
        self.assertIn("PLAYWRIGHT_BROWSERS_PATH", text)
        self.assertIn('ENABLE_BROWSER_E2E}" == "1"', text)

    def test_agents_md_points_at_script(self):
        agents = REPO / "AGENTS.md"
        self.assertTrue(agents.is_file())
        body = agents.read_text(encoding="utf-8")
        self.assertIn("crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh", body)
        self.assertIn(".env", body)
        self.assertIn(".gitlab-ci.yml", body)
        self.assertIn("no runner", body.lower())
        self.assertIn("docs/human-testing.md", body)
        self.assertIn("docs/testing-standardization.md", body)
        self.assertIn("./scripts/human-test.sh", body)
        self.assertIn("peer", body.lower())
        self.assertIn("record", body.lower())
        # Keep committed notes repo-relative (no machine home / checkout path).
        self.assertNotIn("/code/", body)
        self.assertNotIn("/home/", body)

    def test_human_testing_doc_and_recorder_exist(self):
        human = REPO / "docs" / "human-testing.md"
        rec = REPO / "tests" / "browser" / "record_demo.py"
        ident = REPO / "tests" / "browser" / "stack_identity.py"
        script = REPO / "scripts" / "human-test.sh"
        self.assertTrue(ident.is_file(), ident)
        self.assertIn("FLEET_GIT_SHA", ident.read_text(encoding="utf-8"))
        self.assertIn("FLEET_GIT_SHA", (REPO / "Dockerfile").read_text(encoding="utf-8"))
        self.assertIn("FLEET_GIT_SHA", (REPO / "scripts" / "fleet-dev.sh").read_text(encoding="utf-8"))
        std = REPO / "docs" / "testing-standardization.md"
        self.assertTrue(std.is_file(), std)
        self.assertIn("#[cfg(test)]", std.read_text(encoding="utf-8"))
        self.assertTrue(human.is_file(), human)
        self.assertTrue(rec.is_file(), rec)
        self.assertTrue(script.is_file(), script)
        self.assertTrue(script.stat().st_mode & 0o111, "human-test.sh must be executable")
        self.assertIn("stack_identity.py", script.read_text(encoding="utf-8"))
        rec_text = rec.read_text(encoding="utf-8")
        for scene in (
            "landing",
            "auth",
            "fleet",
            "chat",
            "lab",
            "integrations",
        ):
            self.assertIn(f'"{scene}"', rec_text, scene)
        body = human.read_text(encoding="utf-8")
        self.assertIn("agent", body.lower())
        self.assertIn("record_demo.py", body)
        self.assertIn("./scripts/human-test.sh", body)
        r = subprocess.run(["bash", "-n", str(script)], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
