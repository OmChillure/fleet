#!/usr/bin/env python3
"""Lock the fail-closed gates so a missing build cannot go green again.

These are source/behavior contracts, not a live stack run. Live suites call
`stack_identity.py --tools` / `--all` and must fail when dx or this-tree
identity is missing.
"""
from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
IDENT = REPO / "tests" / "browser" / "stack_identity.py"
BOOTSTRAP = REPO / "scripts" / "bootstrap-every-test.sh"
EVERY = REPO / "scripts" / "run-every-test.sh"
FLEET_DEV = REPO / "scripts" / "fleet-dev.sh"
HUMAN = REPO / "scripts" / "human-test.sh"
INSTALL = (
    REPO
    / "crates"
    / "fleet-sandbox-driver"
    / "sandbox-lab"
    / "scripts"
    / "install-full-suite-deps.sh"
)
FULL_SUITE = (
    REPO
    / "crates"
    / "fleet-sandbox-driver"
    / "sandbox-lab"
    / "scripts"
    / "run-full-suite.sh"
)
DOCKERFILE = REPO / "Dockerfile"
DOCKERFILE_UI = REPO / "Dockerfile.ui"
DOCKERFILE_OP = REPO / "Dockerfile.operator"
SERVER_LIB = REPO / "crates" / "fleet-server" / "src" / "lib.rs"
AGENTS = REPO / "AGENTS.md"

FORBIDDEN_PHRASES = (
    "Kind UI is enough",
    "E2e used rebuilt Kind UI",
    "use Kind UI or fix openssl",
)

SCRIPT_PATHS = (BOOTSTRAP, EVERY, FLEET_DEV, HUMAN, INSTALL, FULL_SUITE)


class FailClosedContractTests(unittest.TestCase):
    def test_forbidden_substitute_phrases_absent(self):
        paths = SCRIPT_PATHS + (
            IDENT,
            AGENTS,
            REPO / "docs" / "testing.md",
            REPO / "docs" / "human-testing.md",
            REPO / "docs" / "testing-standardization.md",
        )
        for path in paths:
            text = path.read_text(encoding="utf-8")
            for phrase in FORBIDDEN_PHRASES:
                self.assertNotIn(phrase, text, f"{path.name}: {phrase!r}")
        for path in SCRIPT_PATHS:
            text = path.read_text(encoding="utf-8")
            self.assertNotIn(
                "kubectl set env",
                text,
                f"{path.name} must not patch FLEET_GIT_SHA on a running pod",
            )

    def test_only_install_script_installs_dx(self):
        install = INSTALL.read_text(encoding="utf-8")
        self.assertIn("cargo install dioxus-cli", install)
        self.assertIn("/usr/local/cargo/bin", install)
        self.assertIn("bash -c", install)
        self.assertNotIn('bash -lc "cargo install', install)
        for path in SCRIPT_PATHS:
            if path == INSTALL:
                continue
            text = path.read_text(encoding="utf-8")
            self.assertNotIn(
                "cargo install dioxus-cli",
                text,
                f"{path.name} must send the human to the install script, not cargo-install",
            )
        for path in (
            REPO / "development.md",
            REPO / "README.md",
            REPO / "crates" / "fleet-ui" / "README.md",
        ):
            text = path.read_text(encoding="utf-8")
            self.assertNotIn("cargo install dioxus-cli", text, path.name)
            self.assertIn("install-full-suite-deps.sh", text)

    def test_bootstrap_does_not_install_or_warn_dx(self):
        text = BOOTSTRAP.read_text(encoding="utf-8")
        self.assertNotIn("cargo install dioxus-cli", text)
        self.assertNotIn("WARN: dx", text)
        self.assertNotIn("pip install", text)
        self.assertIn("install-full-suite-deps.sh", text)
        self.assertRegex(text, r"need dx[\s\S]{0,800}exit 1")

    def test_every_test_requires_tools_and_host_dx_build(self):
        text = EVERY.read_text(encoding="utf-8")
        self.assertIn("stack_identity.py", text)
        self.assertIn("--tools", text)
        self.assertIn("dx build", text)
        self.assertIn("FLEET_ALLOW_FOREIGN_STACK", text)
        self.assertIn("exit 1", text)
        self.assertIn("FLEET_ALLOW_FOREIGN_STACK is not a green path", text)
        self.assertNotIn("FLEET_SKIP_UI", text)
        self.assertIn("fleet-git-sha.txt", text)

    def test_fleet_dev_does_not_skip_ui_without_dx(self):
        text = FLEET_DEV.read_text(encoding="utf-8")
        self.assertNotIn("dx not found — skip UI", text)
        self.assertIn("need_cmd dx", text)
        self.assertIn("write_ui_git_sha", text)
        self.assertIn("fleet-git-sha.txt", text)
        self.assertIn("--api-only", text)
        self.assertIn("FLEET_SKIP_UI", text)

    def test_identity_has_no_allow_continue(self):
        text = IDENT.read_text(encoding="utf-8")
        self.assertNotIn("FLEET_ALLOW_FOREIGN_STACK", text)
        self.assertNotIn("continuing", text.lower())
        self.assertIn("fleet-git-sha.txt", text)
        self.assertIn("require_tools", text)
        self.assertIn("--tools", text)
        self.assertIn("--api-only", text)
        self.assertIn("Do not kubectl set env FLEET_GIT_SHA", text)

    def test_orch_worker_url_is_config_not_live_env(self):
        state = (
            REPO / "crates" / "fleet-orchestrator" / "src" / "api" / "state.rs"
        ).read_text(encoding="utf-8")
        self.assertIn("cfg.orchestrator_public_url", state)
        self.assertNotIn(
            "public_orchestrator_url(&cfg.http_addr)",
            state,
            "re-reading API_PUBLIC_BASE at spawn sends workers to Kind :8080",
        )

    def test_health_sha_is_compile_time_only(self):
        lib = SERVER_LIB.read_text(encoding="utf-8")
        self.assertIn('option_env!("FLEET_GIT_SHA")', lib)
        self.assertNotIn(
            'std::env::var("FLEET_GIT_SHA")',
            lib,
            "runtime ENV lets `kubectl set env` fake this tree",
        )
        build_rs = (REPO / "crates" / "fleet-server" / "build.rs").read_text(
            encoding="utf-8"
        )
        self.assertIn("rerun-if-env-changed=FLEET_GIT_SHA", build_rs)

    def test_identity_cannot_be_satisfied_by_kubectl_env_or_foreign_allow(self):
        """Compile-time bake only; kubectl set env / FLEET_ALLOW_FOREIGN_STACK are not green."""
        lib = SERVER_LIB.read_text(encoding="utf-8")
        self.assertIn('option_env!("FLEET_GIT_SHA")', lib)
        self.assertNotIn(
            'std::env::var("FLEET_GIT_SHA")',
            lib,
            "runtime ENV lets `kubectl set env` fake this tree",
        )
        self.assertNotIn(
            'env::var("FLEET_GIT_SHA")',
            lib,
            "use std::env; env::var is the same kubectl-env green path",
        )
        self.assertIn("kubectl set env", lib)

        host = (REPO / "crates" / "fleet-host" / "src" / "software.rs").read_text(
            encoding="utf-8"
        )
        self.assertIn('option_env!("FLEET_GIT_SHA")', host)
        self.assertNotIn('std::env::var("FLEET_GIT_SHA")', host)
        self.assertNotIn('env::var("FLEET_GIT_SHA")', host)

        plane = (
            REPO / "crates" / "fleet-server" / "src" / "worker_hosts.rs"
        ).read_text(encoding="utf-8")
        self.assertIn('option_env!("FLEET_GIT_SHA")', plane)
        self.assertNotIn('std::env::var("FLEET_GIT_SHA")', plane)
        self.assertNotIn('env::var("FLEET_GIT_SHA")', plane)

        build_rs = (REPO / "crates" / "fleet-server" / "build.rs").read_text(
            encoding="utf-8"
        )
        self.assertIn("rerun-if-env-changed=FLEET_GIT_SHA", build_rs)
        self.assertIn("compile-time only", build_rs)
        self.assertIn("kubectl set env", build_rs)

        ident = IDENT.read_text(encoding="utf-8")
        self.assertIn("Do not kubectl set env FLEET_GIT_SHA", ident)
        self.assertIn("tree-id.sh", ident)
        self.assertIn("expected_sha", ident)
        self.assertNotIn("FLEET_ALLOW_FOREIGN_STACK", ident)
        self.assertNotIn('os.environ.get("FLEET_GIT_SHA")', ident)
        self.assertNotIn("os.environ['FLEET_GIT_SHA']", ident)
        self.assertNotIn('os.getenv("FLEET_GIT_SHA")', ident)

        reject = 'ERROR: FLEET_ALLOW_FOREIGN_STACK is not a green path. Unset it.'
        for path in (EVERY, FULL_SUITE):
            text = path.read_text(encoding="utf-8")
            self.assertIn(reject, text, path.name)
            self.assertIn("unset FLEET_ALLOW_FOREIGN_STACK", text, path.name)
            self.assertRegex(
                text,
                r'FLEET_ALLOW_FOREIGN_STACK:-.*" == "1"[\s\S]{0,250}exit 1',
                f"{path.name} must exit 1 when FLEET_ALLOW_FOREIGN_STACK=1",
            )
            self.assertNotIn("kubectl set env", text, path.name)

        for path in (
            AGENTS,
            REPO / "docs" / "testing.md",
            REPO / "docs" / "testing-standardization.md",
        ):
            body = path.read_text(encoding="utf-8")
            self.assertIn("kubectl set env", body, path.name)
            self.assertIn("FLEET_ALLOW_FOREIGN_STACK", body, path.name)
            self.assertIn("green path", body.lower(), path.name)

        human = (REPO / "docs" / "human-testing.md").read_text(encoding="utf-8")
        self.assertIn("FLEET_ALLOW_FOREIGN_STACK", human)
        self.assertIn("no green path", human.lower())

        for yml in (REPO / "infra").rglob("*.yaml"):
            text = yml.read_text(encoding="utf-8")
            self.assertNotIn(
                "FLEET_GIT_SHA",
                text,
                f"{yml.relative_to(REPO)} must not inject runtime FLEET_GIT_SHA",
            )

    def test_dockerfile_bakes_sha_before_cargo(self):
        text = DOCKERFILE.read_text(encoding="utf-8")
        arg_at = text.find("ARG FLEET_GIT_SHA")
        cargo_at = text.find("cargo build")
        self.assertGreaterEqual(arg_at, 0, "Dockerfile must declare ARG FLEET_GIT_SHA")
        self.assertGreaterEqual(cargo_at, 0)
        self.assertLess(
            arg_at,
            cargo_at,
            "FLEET_GIT_SHA must be ARG/ENV before cargo build (compile-time)",
        )
        self.assertIn("LABEL fleet.git_sha", text)
        self.assertIn('test -n "${FLEET_GIT_SHA}"', text)

    def test_compose_requires_sha_build_arg(self):
        text = (REPO / "docker-compose.yml").read_text(encoding="utf-8")
        self.assertGreaterEqual(text.count("FLEET_GIT_SHA"), 2)
        self.assertIn("${FLEET_GIT_SHA:-}", text)
        # `:?` makes `docker compose up postgres` fail before any build.
        self.assertNotIn("${FLEET_GIT_SHA:?", text)
        install = INSTALL.read_text(encoding="utf-8")
        self.assertIn('scripts/tree-id.sh', install)

    def test_ui_image_embeds_sha_file_and_label(self):
        text = DOCKERFILE_UI.read_text(encoding="utf-8")
        self.assertIn("ARG FLEET_GIT_SHA", text)
        self.assertIn("fleet-git-sha.txt", text)
        self.assertIn("LABEL fleet.git_sha", text)
        self.assertIn('test -n "${FLEET_GIT_SHA}"', text)
        nginx = (REPO / "deploy" / "nginx-ui.conf").read_text(encoding="utf-8")
        self.assertIn("fleet-git-sha.txt", nginx)
        self.assertIn("=404", nginx)

    def test_operator_image_labeled(self):
        text = DOCKERFILE_OP.read_text(encoding="utf-8")
        self.assertIn("ARG FLEET_GIT_SHA", text)
        self.assertIn("LABEL fleet.git_sha", text)

    def test_install_installs_dx_and_rebuilds_stale_local_tags(self):
        text = INSTALL.read_text(encoding="utf-8")
        self.assertIn("dioxus-cli", text)
        self.assertIn("install_dx", text)
        self.assertIn("fleet.git_sha", text)
        self.assertIn("wasm32-unknown-unknown", text)
        self.assertIn("stack_identity.py", text)
        self.assertNotRegex(
            text,
            r"skip \$\{tag\} \(exists; FORCE_REBUILD=1 to rebuild\)",
            "must not skip :local tags that do not match HEAD",
        )

    def test_tree_id_lock_requires_rebuild_after_changes(self):
        script = REPO / "scripts" / "tree-id.sh"
        self.assertTrue(script.is_file(), script)
        self.assertTrue(script.stat().st_mode & 0o111, "tree-id.sh must be executable")
        ignore = (REPO / ".gitignore").read_text(encoding="utf-8")
        self.assertIn("__pycache__/", ignore)
        self.assertIn("crates/fleet-ui/public/fleet-git-sha.txt", ignore)
        self.assertIn("tailwind.css", script.read_text(encoding="utf-8"))
        ident = IDENT.read_text(encoding="utf-8")
        self.assertIn("tree-id.sh", ident)
        self.assertIn("Verification without that rebuild is not valid", ident)
        self.assertIn("--api-only", ident)
        self.assertIn("tree-id.sh", EVERY.read_text(encoding="utf-8"))
        self.assertIn("tree-id.sh", INSTALL.read_text(encoding="utf-8"))
        agents = AGENTS.read_text(encoding="utf-8")
        self.assertIn("tree-id.sh", agents)
        self.assertIn("not valid", agents.lower())
        self.assertRegex(
            ident,
            r"\[0-9a-f\]\{40\}\(-\[0-9a-f\]\{12\}\)\?",
        )

        src = script.read_text(encoding="utf-8")
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            (root / "scripts").mkdir()
            (root / "scripts" / "tree-id.sh").write_text(src, encoding="utf-8")
            subprocess.run(["git", "init", "-q"], cwd=td, check=True)
            subprocess.run(
                ["git", "config", "user.email", "tree-id@example.test"],
                cwd=td,
                check=True,
            )
            subprocess.run(
                ["git", "config", "user.name", "tree-id"],
                cwd=td,
                check=True,
            )
            (root / "tracked.txt").write_text("one\n", encoding="utf-8")
            subprocess.run(
                ["git", "add", "scripts/tree-id.sh", "tracked.txt"],
                cwd=td,
                check=True,
            )
            subprocess.run(
                ["git", "commit", "-q", "-m", "init"],
                cwd=td,
                check=True,
            )
            run = lambda: subprocess.check_output(
                ["bash", str(root / "scripts" / "tree-id.sh")],
                text=True,
                cwd=td,
            ).strip()
            clean = run()
            self.assertRegex(clean, r"^[0-9a-f]{40}$", clean)
            (root / "tracked.txt").write_text("two\n", encoding="utf-8")
            dirty1 = run()
            self.assertRegex(dirty1, r"^[0-9a-f]{40}-[0-9a-f]{12}$", dirty1)
            self.assertTrue(dirty1.startswith(clean + "-"), dirty1)
            (root / "tracked.txt").write_text("three\n", encoding="utf-8")
            dirty2 = run()
            self.assertRegex(dirty2, r"^[0-9a-f]{40}-[0-9a-f]{12}$", dirty2)
            self.assertNotEqual(dirty1, dirty2)
            (root / "tracked.txt").write_text("one\n", encoding="utf-8")
            self.assertEqual(run(), clean)

    def test_agents_md_forbids_substitutes(self):
        body = AGENTS.read_text(encoding="utf-8")
        self.assertIn("fail-closed", body.lower())
        self.assertIn("fleet-git-sha.txt", body)
        self.assertRegex(body.lower(), r"host [`']?dx[`']?")
        self.assertIn("kubectl set env", body)
        self.assertIn("not a substitute", body.lower())
        self.assertIn("FLEET_ALLOW_FOREIGN_STACK", body)
        self.assertIn("option_env!", body)
        self.assertIn("green path", body.lower())

    def test_agents_md_test_writer_is_independent_not_full_rerun(self):
        body = AGENTS.read_text(encoding="utf-8")
        testing = (REPO / "docs" / "testing.md").read_text(encoding="utf-8")
        self.assertIn("Treat each test as **independent**", body)
        self.assertIn("Do **not** re-run the entire suite on every iteration", body)
        self.assertIn("harvest-runner-catalog.sh", body)
        self.assertIn("harvest-runner-catalog.sh", testing)
        harvest = REPO / "scripts" / "harvest-runner-catalog.sh"
        self.assertTrue(harvest.is_file(), harvest)
        script = harvest.read_text(encoding="utf-8")
        self.assertIn(".env", script)

    def test_tools_gate_fails_when_dx_absent(self):
        with tempfile.TemporaryDirectory() as empty:
            env = os.environ.copy()
            env["PATH"] = empty
            r = subprocess.run(
                [sys.executable, str(IDENT), "--tools"],
                cwd=str(REPO),
                capture_output=True,
                text=True,
                env=env,
                check=False,
            )
            self.assertNotEqual(r.returncode, 0, r.stdout + r.stderr)
            err = r.stderr + r.stdout
            self.assertIn("MISSING_TOOLS", err)
            self.assertRegex(err, r"\bdx\b")
            self.assertNotIn("WARN:", err)

    def test_human_and_every_do_not_set_skip_ui(self):
        for path in (HUMAN, EVERY, FULL_SUITE):
            text = path.read_text(encoding="utf-8")
            self.assertNotIn("FLEET_SKIP_UI=1", text, path.name)
            self.assertNotIn("FLEET_ALLOW_FOREIGN_STACK=1", text, path.name)

    def test_full_suite_rejects_foreign_allow(self):
        text = FULL_SUITE.read_text(encoding="utf-8")
        self.assertIn("FLEET_ALLOW_FOREIGN_STACK", text)
        self.assertIn("stack_identity.py", text)
        self.assertIn("FLEET_ALLOW_FOREIGN_STACK is not a green path", text)
        self.assertIn("exit 1", text)

    def test_bootstrap_postgres_wait_is_fail_closed(self):
        text = BOOTSTRAP.read_text(encoding="utf-8")
        self.assertIn("postgres :5433 did not become ready", text)
        self.assertRegex(text, r"ready=0[\s\S]{0,400}exit 1")

    def test_install_does_not_skip_unavailable_required_rpms(self):
        text = INSTALL.read_text(encoding="utf-8")
        self.assertNotRegex(text, r"dnf install -y --skip-unavailable")
        self.assertIn("ENABLE_LMSTUDIO_PROXY=1 but", text)
        self.assertIn("fleet.harness_src", text)
        self.assertIn("_pm_install dnf valkey-compat-redis redis", text)
        self.assertIn("_pm_install dnf docker-compose-plugin docker-compose", text)
        self.assertIn("_pm_install dnf moby-engine docker", text)

    def test_host_dx_build_is_portable(self):
        text = EVERY.read_text(encoding="utf-8")
        self.assertNotIn("xargs -r", text)
        self.assertIn('public="$(dirname "${html}")"', text)

    def test_ui_dockerfile_sha_after_dx_install(self):
        text = DOCKERFILE_UI.read_text(encoding="utf-8")
        install_at = text.find("cargo install dioxus-cli")
        arg_at = text.find("ARG FLEET_GIT_SHA")
        self.assertGreaterEqual(install_at, 0)
        self.assertGreater(
            arg_at,
            install_at,
            "builder ARG FLEET_GIT_SHA must come after cargo install dioxus-cli",
        )

    def test_lab_env_honors_profile_full_defaults(self):
        lab_env = (
            REPO
            / "crates"
            / "fleet-sandbox-driver"
            / "sandbox-lab"
            / "tests"
            / "lab_env.py"
        )
        text = lab_env.read_text(encoding="utf-8")
        self.assertIn("PROFILE_DEFAULTS", text)
        self.assertIn('"ENABLE_KIND": True', text)
        self.assertIn("SKIP_KIND", text)
        src = REPO / "crates" / "fleet-sandbox-driver" / "sandbox-lab" / "tests"
        r = subprocess.run(
            [sys.executable, "-m", "unittest", "test_lab_env", "-v"],
            cwd=str(src),
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
