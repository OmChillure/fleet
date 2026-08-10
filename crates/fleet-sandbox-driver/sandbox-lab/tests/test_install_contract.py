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
        # Deleted caller cwd (/tmp worktree) must not break rustup/cargo.
        self.assertIn('cd "${REPO}"', text)

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

    def test_header_lists_compatible_systems_and_tested(self):
        text = SCRIPT.read_text(encoding="utf-8")
        head = text.split("OS matrix", 1)[0]
        self.assertIn("Compatible systems", head)
        self.assertIn("TESTED (this checkout, 2026-08-09)", head)
        self.assertIn("Fedora", head)
        for token in (
            "Debian / Ubuntu   not tested",
            "Arch / Manjaro    not tested",
            "openSUSE          not tested",
            "Alpine            not tested",
            "Homebrew          not tested",
            "Windows WSL2",
            "Windows cmd / PowerShell        not supported",
        ):
            self.assertIn(token, head, token)
        self.assertNotIn("Ubuntu            TESTED", head)

    def test_no_crate_version_pin_in_script(self):
        text = SCRIPT.read_text(encoding="utf-8")
        self.assertNotIn("enum-ordinalize", text)

    def test_kind_rollout_after_load(self):
        text = SCRIPT.read_text(encoding="utf-8")
        load_at = text.find("kind load docker-image")
        restart_at = text.find("rollout restart deploy/api")
        wait_ready_at = text.find("wait --for=condition=Ready pods --all")
        self.assertGreaterEqual(load_at, 0)
        self.assertGreater(restart_at, load_at, "must restart deploys after kind load")
        self.assertIn("rollout status deploy/api", text)
        self.assertIn("rollout status deploy/ui", text)
        # Ready wait on pre-existing pods is not a this-tree check.
        self.assertEqual(wait_ready_at, -1, "do not wait Ready on possibly-old pods")

    def test_docker_pull_retries_and_uses_local_cache(self):
        text = SCRIPT.read_text(encoding="utf-8")
        self.assertIn("docker_pull_or_have", text)
        self.assertIn("attempt ${i}/3", text)
        self.assertIn("already local", text)
        self.assertNotIn(
            "\ndocker pull redis:7-alpine\n",
            text if text.endswith("\n") else text + "\n",
            "bare docker pull must not abort install when redis:7-alpine is already local",
        )

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
        self.assertIn("install_playwright_os_libs", text)
        self.assertIn("_pm_install", text)
        self.assertIn("playwright install-deps chromium", text)
        # Matrix: every documented Linux PM + darwin/windows no-op.
        for token in (
            "pm=apt",
            "pm=dnf",
            "pm=pacman",
            "pm=zypper",
            "pm=apk",
            "darwin)",
            "windows)",
            "mesa-libgbm",
            "mozilla-nss",
            "gtk+3.0",
            "libasound2t64",
        ):
            self.assertIn(token, text, token)
        # Required libs are not a single --skip-unavailable dump.
        pw = text.split("install_playwright_os_libs()", 1)[1]
        pw = pw.split("install_packages", 1)[0]
        self.assertNotIn("--skip-unavailable", pw)
        self.assertIn("chromium.launch(headless=True)", text)
        stub = REPO / "scripts" / "sudo-playwright-deps.sh"
        stub_text = stub.read_text(encoding="utf-8")
        self.assertIn("install-full-suite-deps.sh", stub_text)
        self.assertNotIn("python3 -m playwright install-deps", stub_text)

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
        self.assertIn("when: never", body)
        ci = (REPO / ".gitlab-ci.yml").read_text(encoding="utf-8")
        self.assertIn("when: never", ci)
        self.assertNotRegex(ci, r"(?m)^(test:|script:|image:)")
        self.assertIn("docs/human-testing.md", body)
        self.assertIn("docs/testing-standardization.md", body)
        self.assertIn("./scripts/human-test.sh", body)
        self.assertIn("peer", body.lower())
        self.assertIn("record", body.lower())
        # Agents must keep using parallelism/subagents when efficient.
        self.assertIn("parallelism", body.lower())
        self.assertIn("subagents", body.lower())
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
        self.assertIn("fleet-git-sha.txt", ident.read_text(encoding="utf-8"))
        self.assertNotIn("FLEET_ALLOW_FOREIGN_STACK", ident.read_text(encoding="utf-8"))
        self.assertIn("FLEET_GIT_SHA", (REPO / "Dockerfile").read_text(encoding="utf-8"))
        self.assertIn("FLEET_GIT_SHA", (REPO / "Dockerfile.ui").read_text(encoding="utf-8"))
        self.assertIn("FLEET_GIT_SHA", (REPO / "scripts" / "fleet-dev.sh").read_text(encoding="utf-8"))
        inst = SCRIPT.read_text(encoding="utf-8")
        self.assertIn("dioxus-cli", inst)
        self.assertIn("install_dx", inst)
        self.assertIn("stack_identity.py", inst)
        self.assertIn("fleet.git_sha", inst)
        self.assertIn("/usr/local/cargo/bin", inst)
        self.assertNotIn('bash -lc "cargo install', inst)
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

    def test_required_packages_are_not_skip_unavailable(self):
        text = SCRIPT.read_text(encoding="utf-8")
        self.assertNotRegex(
            text,
            r"dnf install -y --skip-unavailable",
            "required rpms must not be skip-unavailable",
        )
        self.assertIn("docker-compose-v2", text)
        self.assertNotIn(
            "redis-server || \\",
            text.replace("\n", " "),
        )
        # Thinner apt fallback removed: second list used to drop redis/libpq/acl.
        self.assertIn("python3-redis", text)
        self.assertIn("libpq-dev", text)
        self.assertIn("_pm_install dnf valkey-compat-redis redis", text)
        self.assertIn("_pm_install dnf docker-compose-plugin docker-compose", text)
        self.assertIn("_pm_install dnf moby-engine docker", text)

    def test_lmstudio_proxy_is_fail_closed(self):
        text = SCRIPT.read_text(encoding="utf-8")
        self.assertIn("ENABLE_LMSTUDIO_PROXY=1 but", text)
        self.assertIn("http://127.0.0.1:1234/v1/models is not up", text)

    def test_harness_images_skip_only_on_src_hash(self):
        text = SCRIPT.read_text(encoding="utf-8")
        self.assertIn("fleet.harness_src", text)
        self.assertIn("harness_src_hash", text)

    def test_does_not_copy_host_grok_auth_into_lab(self):
        text = SCRIPT.read_text(encoding="utf-8")
        self.assertIn('rm -f "${LAB}/.grok-auth.json"', text)
        self.assertNotIn('cp -a "${REAL_HOME}/.grok/auth.json" "${LAB}/.grok-auth.json"', text)

    def test_kubectl_kind_version_not_swallowed(self):
        text = SCRIPT.read_text(encoding="utf-8")
        self.assertNotIn("kubectl version --client=true || true", text)
        self.assertNotIn("kind version || true", text)

    def test_ollama_reuses_healthy_listener(self):
        text = SCRIPT.read_text(encoding="utf-8")
        self.assertIn("ollama already listening", text)
        self.assertIn("not restarting", text)
        self.assertIn("reset-failed", text)
        self.assertIn("ollama_listen_is_public", text)
        self.assertIn("ENABLE_OLLAMA=1 but http://127.0.0.1:11434/api/tags is not up", text)
        # Unconditional bounce of a healthy unit is what hit start-limit-hit.
        restart_at = text.find("systemctl restart ollama")
        self.assertEqual(
            restart_at,
            -1,
            "must not systemctl restart ollama on every install",
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
