#!/usr/bin/env python3
"""Lock the fail-closed gates so a missing build cannot go green again.

These are source/behavior contracts, not a live stack run. Live suites call
`stack_identity.py --tools` / `--all` and must fail when dx or this-tree
identity is missing.
"""
from __future__ import annotations

import importlib.util
import json
import os
import re
import shlex
import subprocess
import sys
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
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
GITLAB_CI = REPO / ".gitlab-ci.yml"

# Comment-stripped body. A job, include, or missing file opens a pending
# pipeline (no runner) and blocks merge. Auto DevOps is the missing-file path.
GITLAB_CI_ACTIVE = "workflow:\n  rules:\n    - when: never\n"
GITLAB_CI_FORBIDDEN_TOP = (
    "include",
    "stages",
    "variables",
    "default",
    "image",
    "services",
    "cache",
    "before_script",
    "after_script",
    "pages",
    "spec",
)


# :local skip/rebuild. Extract the product function; do not reimplement it.
_MOCK_DOCKER = r"""#!/usr/bin/env bash
set -euo pipefail
STATE="${MOCK_DOCKER_STATE:?}"
if [[ "${1:-}" == "image" && "${2:-}" == "inspect" ]]; then
  shift 2
  fmt=""
  tag=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      -f)
        fmt="${2:-}"
        shift 2
        ;;
      *)
        tag="$1"
        shift
        ;;
    esac
  done
  safe="${tag//\//__}"
  safe="${safe//:/__}"
  if [[ ! -e "${STATE}/images/${safe}" ]]; then
    exit 1
  fi
  if [[ -n "${fmt}" ]]; then
    if [[ -f "${STATE}/images/${safe}.label" ]]; then
      cat "${STATE}/images/${safe}.label"
    fi
  fi
  exit 0
fi
if [[ "${1:-}" == "build" ]]; then
  printf '%s\n' "$*" >> "${STATE}/builds"
  exit 0
fi
echo "unexpected docker: $*" >&2
exit 1
"""


def _bash_fn(text: str, name: str) -> str:
    """Product function body. Closing `}` must be at column 0."""
    m = re.search(rf"(?ms)^{re.escape(name)}\(\) \{{$.*?\n\}}", text)
    if m:
        return m.group(0)
    m = re.search(rf"(?m)^{re.escape(name)}\(\) \{{[^}}]*\}}", text)
    if m:
        return m.group(0)
    raise AssertionError(f"missing {name}()")


def _gitlab_ci_active(text: str) -> str:
    lines: list[str] = []
    for raw in text.replace("\r\n", "\n").replace("\r", "\n").split("\n"):
        stripped = raw.split("#", 1)[0].rstrip()
        if stripped.strip():
            lines.append(stripped)
    return ("\n".join(lines) + "\n") if lines else ""


def _gitlab_ci_top_level_keys(active: str) -> list[str]:
    keys: list[str] = []
    for line in active.splitlines():
        if not line or line[0] in " \t-":
            continue
        if ":" not in line:
            continue
        keys.append(line.split(":", 1)[0].strip())
    return keys

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

    def test_fleet_dev_remaps_when_kind_owns_ports(self):
        """Kind extraPortMappings have no host pid; lsof-only miss them."""
        text = FLEET_DEV.read_text(encoding="utf-8")
        self.assertIn("port_is_open", text)
        self.assertIn("ss -ltn", text)
        self.assertIn("choose_host_ports", text)
        self.assertIn("18080", text)
        self.assertIn("13030", text)
        self.assertIn('PORT="$port"', text)
        self.assertIn("FLEET_API_URL", text)
        self.assertIn("write_dioxus_proxy", text)
        self.assertIn("restore_dioxus_proxy", text)
        self.assertIn("Dioxus.toml", text)
        dockerignore = (REPO / ".dockerignore").read_text(encoding="utf-8")
        self.assertIn("crates/fleet-ui/public/fleet-git-sha.txt", dockerignore)

    def test_identity_has_no_allow_continue(self):
        text = IDENT.read_text(encoding="utf-8")
        self.assertNotIn("FLEET_ALLOW_FOREIGN_STACK", text)
        self.assertNotIn("continuing", text.lower())
        self.assertIn("fleet-git-sha.txt", text)
        self.assertIn("require_tools", text)
        self.assertIn("--tools", text)
        self.assertIn("--api-only", text)
        self.assertIn("--repo", text)
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
        self.assertIn("--repo", ident)
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

    def test_live_identity_fails_on_mismatch_despite_kubectl_env_and_foreign_allow(self):
        """Live e2e must fail when /health.git_sha != tree-id.

        kubectl set env FLEET_GIT_SHA and FLEET_ALLOW_FOREIGN_STACK=1 are not
        a green path. Identity is scripts/tree-id.sh vs the compile-time bake.

        The CLI check uses an isolated git worktree (`--repo`). A shared dirty
        checkout changes tree-id between snapshot and probe; that is not a
        matching bake, and FLEET_GIT_SHA env still must not win.
        """
        spec = importlib.util.spec_from_file_location("stack_identity", IDENT)
        self.assertIsNotNone(spec)
        self.assertIsNotNone(spec.loader)
        ident = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(ident)

        foreign = "a" * 40
        prev_sha = os.environ.get("FLEET_GIT_SHA")
        prev_allow = os.environ.get("FLEET_ALLOW_FOREIGN_STACK")
        os.environ["FLEET_GIT_SHA"] = foreign
        os.environ["FLEET_ALLOW_FOREIGN_STACK"] = "1"
        health = {"sha": foreign}

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                if self.path.split("?", 1)[0] != "/health":
                    self.send_error(404)
                    return
                body = json.dumps(
                    {
                        "status": "ok",
                        "service": "fleet-server",
                        "db": "up",
                        "git_sha": health["sha"],
                        "apis": ["auth", "workspaces", "chat"],
                    }
                ).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                return

        httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=httpd.serve_forever, daemon=True)
        thread.start()
        url = f"http://127.0.0.1:{httpd.server_address[1]}"
        try:
            with tempfile.TemporaryDirectory() as td:
                root = Path(td)
                (root / "scripts").mkdir()
                (root / "scripts" / "tree-id.sh").write_text(
                    (REPO / "scripts" / "tree-id.sh").read_text(encoding="utf-8"),
                    encoding="utf-8",
                )
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

                want = ident.expected_sha(root)
                self.assertRegex(want, r"^[0-9a-f]{40}(-[0-9a-f]{12})?$")
                self.assertNotEqual(want, foreign)

                # Process-env bake must not replace tree-id.sh (kubectl set env).
                self.assertEqual(ident.expected_sha(root), want)
                self.assertNotEqual(ident.expected_sha(root), foreign)

                with self.assertRaises(ident.ForeignStackError) as ctx:
                    ident.require_this_tree(root, url, require_ui=False)
                err = str(ctx.exception)
                self.assertIn(foreign, err)
                self.assertIn(want, err)
                self.assertIn("kubectl set env", err)
                self.assertIn("/health.git_sha", err)

                env = os.environ.copy()
                env["FLEET_API_URL"] = url
                env["FLEET_GIT_SHA"] = foreign
                env["FLEET_ALLOW_FOREIGN_STACK"] = "1"
                ran = subprocess.run(
                    [
                        sys.executable,
                        str(IDENT),
                        "--api-only",
                        "--repo",
                        str(root),
                    ],
                    cwd=str(REPO),
                    capture_output=True,
                    text=True,
                    env=env,
                    check=False,
                )
                self.assertNotEqual(ran.returncode, 0, ran.stdout + ran.stderr)
                out = ran.stderr + ran.stdout
                self.assertIn("FOREIGN_STACK", out)
                self.assertIn(foreign, out)
                self.assertIn(want, out)
                self.assertNotIn("THIS_TREE", out)
                self.assertNotIn("WARN:", out)

                # Control: matching bake still passes so this is not "every health fails".
                health["sha"] = want
                body = ident.require_this_tree(root, url, require_ui=False)
                self.assertEqual(body.get("git_sha"), want)
                env["FLEET_GIT_SHA"] = "b" * 40
                env["FLEET_ALLOW_FOREIGN_STACK"] = "1"
                ok = subprocess.run(
                    [
                        sys.executable,
                        str(IDENT),
                        "--api-only",
                        "--repo",
                        str(root),
                    ],
                    cwd=str(REPO),
                    capture_output=True,
                    text=True,
                    env=env,
                    check=False,
                )
                self.assertEqual(ok.returncode, 0, ok.stdout + ok.stderr)
                self.assertIn("THIS_TREE", ok.stdout)
                self.assertIn(want, ok.stdout)
        finally:
            httpd.shutdown()
            httpd.server_close()
            if prev_sha is None:
                os.environ.pop("FLEET_GIT_SHA", None)
            else:
                os.environ["FLEET_GIT_SHA"] = prev_sha
            if prev_allow is None:
                os.environ.pop("FLEET_ALLOW_FOREIGN_STACK", None)
            else:
                os.environ["FLEET_ALLOW_FOREIGN_STACK"] = prev_allow

        e2e = (REPO / "tests" / "browser" / "test_app_e2e.py").read_text(
            encoding="utf-8"
        )
        self.assertIn("require_this_tree", e2e)
        self.assertNotIn("FLEET_ALLOW_FOREIGN_STACK", e2e)
        self.assertNotIn("pytest.skip", e2e)
        self.assertNotIn("unittest.skip", e2e)

    def test_dockerfile_bakes_sha_on_consuming_compile(self):
        """Dep fetch/warm may precede ARG. Sha-consuming cargo must see ARG/ENV.

        Tree-id is identity, not the crates.io cache key. A dirty suffix must
        not force `Downloading crates` / kube-sqlx-zenoh rebuild.
        """
        text = DOCKERFILE.read_text(encoding="utf-8")
        builder, sep, _runtime = text.partition("\nFROM debian")
        self.assertTrue(sep, "Dockerfile must have a debian runtime stage")
        self.assertIn("cargo fetch --locked", builder)
        self.assertIn("-p fleet-server -p fleet-host -p fleet-driver", builder)
        self.assertIn("--mount=type=cache,target=/usr/local/cargo/registry", builder)
        self.assertIn("--mount=type=cache,target=/app/target", builder)
        first_cargo = builder.find("cargo fetch")
        arg_at = builder.rfind("ARG FLEET_GIT_SHA")
        last_cargo = builder.rfind("cargo build")
        self.assertGreaterEqual(first_cargo, 0)
        self.assertGreaterEqual(arg_at, 0, "builder must declare ARG FLEET_GIT_SHA")
        self.assertLess(
            first_cargo,
            arg_at,
            "dep cargo fetch/warm must precede ARG so tree-id is not the cache key",
        )
        self.assertLess(
            arg_at,
            last_cargo,
            "sha-consuming cargo build must follow ARG/ENV (compile-time bake)",
        )
        self.assertIn("ENV FLEET_GIT_SHA=${FLEET_GIT_SHA}", builder)
        self.assertIn("LABEL fleet.git_sha", text)
        self.assertIn('test -n "${FLEET_GIT_SHA}"', text)
        self.assertIn("COPY runner-packages.toml", text)
        # Runtime must not inject identity (kubectl set env is not a green path).
        runtime = text[text.rfind("\nFROM ") :]
        self.assertNotRegex(
            runtime,
            r"(?m)^ENV FLEET_GIT_SHA=",
            "runtime must not inject identity (kubectl set env is not a green path)",
        )

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

    def test_local_images_skip_rebuild_only_when_label_matches_tree_id(self):
        """Stale :local must rebuild. Skip only if label fleet.git_sha == tree-id.

        Matching label may skip. Existence of the tag, leftover FLEET_GIT_SHA,
        or `git rev-parse HEAD` (no dirty suffix) is not enough.
        """
        tree = "0123456789abcdef0123456789abcdef01234567"
        dirty = tree + "-aabbccddeeff"
        stale = "ffffffffffffffffffffffffffffffffffffffff"
        local_tags = (
            "agent-fleet-api:local",
            "agent-fleet-ui:local",
            "agent-fleet-operator:local",
        )

        for path in (DOCKERFILE, DOCKERFILE_UI, DOCKERFILE_OP):
            text = path.read_text(encoding="utf-8")
            last_from = text.rfind("\nFROM ")
            if last_from < 0:
                last_from = 0 if text.startswith("FROM ") else -1
            self.assertGreaterEqual(last_from, 0, f"{path.name}: missing FROM")
            label_at = text.rfind("LABEL fleet.git_sha")
            self.assertGreater(
                label_at,
                last_from,
                f"{path.name}: inspectable LABEL fleet.git_sha must be on the runtime stage",
            )
            runtime = text[last_from:]
            self.assertIn("ARG FLEET_GIT_SHA", runtime, path.name)
            self.assertIn("LABEL fleet.git_sha=${FLEET_GIT_SHA}", runtime, path.name)
            self.assertIn('test -n "${FLEET_GIT_SHA}"', runtime, path.name)

        every = EVERY.read_text(encoding="utf-8")
        prepare = _bash_fn(every, "prepare_kind")
        build_fn_every = _bash_fn(every, "docker_build_if_needed")
        label_fn = _bash_fn(every, "image_label_sha")
        self.assertIn("tree-id.sh", prepare)
        self.assertNotIn("git rev-parse HEAD", prepare)
        self.assertIn('sha="$(bash "${ROOT}/scripts/tree-id.sh")"', prepare)
        self.assertIn('export FLEET_GIT_SHA="${sha}"', prepare)
        self.assertIn('index .Config.Labels "fleet.git_sha"', label_fn)
        self.assertIn("image_label_sha", build_fn_every)
        self.assertRegex(
            build_fn_every,
            r'\[\[ "\$\{have\}" == "\$\{sha\}" \]\]',
            "prepare_kind skip compare must be inspect label == tree-id.sh",
        )
        self.assertIn('--build-arg "FLEET_GIT_SHA=${sha}"', build_fn_every)
        for tag in local_tags:
            self.assertIn(f"docker_build_if_needed {tag}", prepare, tag)
            self.assertIn(f'-t "${{tag}}"', build_fn_every)
        # Existence-only skip is a lockout on a dirty tree / old bake.
        self.assertNotRegex(
            prepare,
            r"skip \$\{tag\} \(exists",
            "prepare_kind must not skip :local just because the tag exists",
        )
        self.assertNotRegex(
            prepare,
            r"docker image inspect[^\n]*\n[^\n]*skip",
            "prepare_kind must not skip :local on inspect-success without fleet.git_sha",
        )
        if re.search(r"\bskip\b", build_fn_every):
            self.assertIn("fleet.git_sha", build_fn_every)
            self.assertIn("tree-id.sh", build_fn_every)
            self.assertIn("image_label_sha", build_fn_every)
        if re.search(r"skip load", prepare):
            self.assertIn("stack_identity.py", prepare)

        install = INSTALL.read_text(encoding="utf-8")
        build_fn = _bash_fn(install, "docker_build_if_needed")
        self.assertIn("tree-id.sh", build_fn)
        self.assertIn("fleet.git_sha", build_fn)
        self.assertNotIn("git rev-parse HEAD", build_fn)
        self.assertIn('sha="$(bash "${REPO}/scripts/tree-id.sh")"', build_fn)
        self.assertIn('index .Config.Labels "fleet.git_sha"', build_fn)
        self.assertRegex(
            build_fn,
            r'\[\[ "\$\{have\}" == "\$\{sha\}" \]\]',
            "skip compare must be inspect label == tree-id.sh, not env or image id",
        )
        self.assertNotIn("${FLEET_GIT_SHA}", build_fn)
        self.assertNotRegex(
            build_fn,
            r"skip \$\{tag\} \(exists; FORCE_REBUILD=1 to rebuild\)",
        )
        for tag in local_tags:
            self.assertIn(
                f"docker_build_if_needed {tag}",
                install,
                f"install must route {tag} through docker_build_if_needed",
            )
        have_fn = 'have_image() { docker image inspect "$1" >/dev/null 2>&1; }'
        self.assertIn(have_fn, install)

        agents = AGENTS.read_text(encoding="utf-8")
        self.assertIn("fleet.git_sha", agents)
        self.assertIn("tree-id.sh", agents)
        self.assertRegex(
            agents,
            r":local.*skip rebuild only when label `fleet\.git_sha` matches",
        )

        def run_build(
            *,
            tree_id: str,
            exists: bool,
            label: str | None,
            force: bool = False,
            env_sha: str | None = None,
        ) -> tuple[subprocess.CompletedProcess[str], str]:
            with tempfile.TemporaryDirectory() as td:
                root = Path(td)
                repo = root / "repo"
                (repo / "scripts").mkdir(parents=True)
                (repo / "scripts" / "tree-id.sh").write_text(
                    "#!/usr/bin/env bash\n"
                    f"printf '%s\\n' {shlex.quote(tree_id)}\n",
                    encoding="utf-8",
                )
                (repo / "scripts" / "tree-id.sh").chmod(0o755)
                state = root / "state"
                (state / "images").mkdir(parents=True)
                tag = "agent-fleet-api:local"
                safe = tag.replace("/", "__").replace(":", "__")
                if exists:
                    (state / "images" / safe).write_text("1\n", encoding="utf-8")
                    if label is not None:
                        (state / "images" / f"{safe}.label").write_text(
                            label, encoding="utf-8"
                        )
                bin_dir = root / "bin"
                bin_dir.mkdir()
                docker = bin_dir / "docker"
                docker.write_text(_MOCK_DOCKER, encoding="utf-8")
                docker.chmod(0o755)
                wrapper = root / "run.sh"
                wrapper.write_text(
                    "#!/usr/bin/env bash\n"
                    "set -euo pipefail\n"
                    f"REPO={shlex.quote(str(repo))}\n"
                    f"FORCE_REBUILD={1 if force else 0}\n"
                    f"{have_fn}\n"
                    f"{build_fn}\n"
                    f"docker_build_if_needed {shlex.quote(tag)} -f Dockerfile .\n",
                    encoding="utf-8",
                )
                env = os.environ.copy()
                env["PATH"] = f"{bin_dir}{os.pathsep}{env.get('PATH', '')}"
                env["MOCK_DOCKER_STATE"] = str(state)
                if env_sha is None:
                    env.pop("FLEET_GIT_SHA", None)
                else:
                    env["FLEET_GIT_SHA"] = env_sha
                ran = subprocess.run(
                    ["bash", str(wrapper)],
                    cwd=str(repo),
                    capture_output=True,
                    text=True,
                    env=env,
                    check=False,
                )
                built = ""
                builds = state / "builds"
                if builds.is_file():
                    built = builds.read_text(encoding="utf-8")
                return ran, built

        def assert_rebuild(built: str, tree_id: str, why: str) -> None:
            self.assertIn("build", built, why)
            self.assertIn(f"FLEET_GIT_SHA={tree_id}", built, why)
            self.assertIn("-t agent-fleet-api:local", built, why)

        # Missing :local → rebuild.
        ran, built = run_build(tree_id=tree, exists=False, label=None)
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        assert_rebuild(built, tree, "missing :local must rebuild")

        # Matching label may skip.
        ran, built = run_build(tree_id=tree, exists=True, label=tree)
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        self.assertEqual(built, "", "matching fleet.git_sha may skip rebuild")
        self.assertIn("skip", ran.stdout)
        self.assertIn("fleet.git_sha", ran.stdout)

        # Dirty tree-id vs clean-HEAD label is stale (git rev-parse HEAD is not the id).
        ran, built = run_build(tree_id=dirty, exists=True, label=tree)
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        assert_rebuild(built, dirty, "dirty tree-id vs clean label must rebuild")
        self.assertIn("rebuild", ran.stdout)

        # Clean tree vs dirty label is stale.
        ran, built = run_build(tree_id=tree, exists=True, label=dirty)
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        assert_rebuild(built, tree, "clean tree vs dirty label must rebuild")

        # Old sha / empty / unset label must rebuild.
        for have in (stale, "", None):
            ran, built = run_build(tree_id=tree, exists=True, label=have)
            self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
            assert_rebuild(
                built, tree, f"stale/empty/unset label {have!r} must rebuild"
            )

        # FORCE_REBUILD=1 rebuilds even on match.
        ran, built = run_build(tree_id=tree, exists=True, label=tree, force=True)
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        assert_rebuild(built, tree, "FORCE_REBUILD=1 must rebuild a matching label")

        # Leftover FLEET_GIT_SHA env is not the compare (kubectl / .env leftover).
        ran, built = run_build(
            tree_id=tree, exists=True, label=stale, env_sha=stale
        )
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        assert_rebuild(
            built, tree, "label matching leftover FLEET_GIT_SHA env must still rebuild"
        )
        ran, built = run_build(
            tree_id=tree, exists=True, label=tree, env_sha=stale
        )
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        self.assertEqual(
            built,
            "",
            "skip compare is tree-id.sh, not FLEET_GIT_SHA env",
        )

    def test_tree_id_lock_requires_rebuild_after_changes(self):
        script = REPO / "scripts" / "tree-id.sh"
        self.assertTrue(script.is_file(), script)
        self.assertTrue(script.stat().st_mode & 0o111, "tree-id.sh must be executable")
        ignore = (REPO / ".gitignore").read_text(encoding="utf-8")
        self.assertIn("__pycache__/", ignore)
        self.assertIn("crates/fleet-ui/public/fleet-git-sha.txt", ignore)
        self.assertIn("tailwind.css", script.read_text(encoding="utf-8"))
        self.assertIn("Dioxus.toml", script.read_text(encoding="utf-8"))
        ident = IDENT.read_text(encoding="utf-8")
        self.assertIn("tree-id.sh", ident)
        self.assertIn("Verification without that rebuild is not valid", ident)
        self.assertIn("--api-only", ident)
        self.assertIn("--repo", ident)
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
        fetch_at = text.find("cargo fetch")
        arg_at = text.find("ARG FLEET_GIT_SHA")
        dx_at = text.find("&& dx build")
        self.assertGreaterEqual(install_at, 0)
        self.assertGreater(
            arg_at,
            install_at,
            "builder ARG FLEET_GIT_SHA must come after cargo install dioxus-cli",
        )
        self.assertGreater(arg_at, fetch_at, "cargo fetch must precede ARG")
        self.assertGreater(dx_at, arg_at, "dx build must see FLEET_GIT_SHA")
        self.assertIn("--mount=type=cache,target=/usr/local/cargo/registry", text)

    def test_operator_compiles_before_sha_label(self):
        text = DOCKERFILE_OP.read_text(encoding="utf-8")
        builder, sep, runtime = text.partition("\nFROM debian")
        self.assertTrue(sep)
        self.assertIn("cargo fetch --locked", builder)
        self.assertIn("-p fleet-operator --bin fleet-operator", builder)
        self.assertIn("--mount=type=cache,target=/usr/local/cargo/registry", builder)
        self.assertLess(builder.find("cargo build"), builder.find("ARG FLEET_GIT_SHA") if "ARG FLEET_GIT_SHA" in builder else len(builder))
        self.assertIn("ARG FLEET_GIT_SHA", runtime)
        self.assertIn("LABEL fleet.git_sha=${FLEET_GIT_SHA}", runtime)
        self.assertNotIn("ENV FLEET_GIT_SHA", runtime)

    def test_gitlab_ci_stays_workflow_when_never(self):
        """No runner: .gitlab-ci.yml must be only workflow.rules when: never."""
        self.assertTrue(
            GITLAB_CI.is_file(),
            "missing .gitlab-ci.yml lets Auto DevOps create a pending pipeline",
        )
        text = GITLAB_CI.read_text(encoding="utf-8")
        active = _gitlab_ci_active(text)
        self.assertEqual(
            active,
            GITLAB_CI_ACTIVE,
            ".gitlab-ci.yml must be only workflow.rules when: never "
            "(comments stripped); a real job blocks merge",
        )
        keys = _gitlab_ci_top_level_keys(active)
        self.assertEqual(keys, ["workflow"], f"unexpected top-level keys: {keys}")
        self.assertIn("when: never", active)
        for key in GITLAB_CI_FORBIDDEN_TOP:
            self.assertNotIn(
                key,
                keys,
                f"top-level {key!r} is a job or pipeline config; do not add it",
            )
        for line in active.splitlines():
            if line.startswith(" ") or line.startswith("\t"):
                continue
            self.assertFalse(
                line.split(":", 1)[0].strip()
                in {
                    "include",
                    "script",
                    "image",
                    "stages",
                    "pages",
                    "trigger",
                    "extends",
                    "rules",
                },
                f"job / Auto DevOps key at column 0: {line!r}",
            )
        lowered = active.lower()
        self.assertNotIn("auto-devops", lowered)
        self.assertNotIn("auto_devops", lowered)
        self.assertNotIn("include:", lowered)
        self.assertNotIn("script:", lowered)
        self.assertNotRegex(
            active,
            r"(?m)^(test:|build:|lint:|deploy:|pages:)",
            "a named job key creates a pending pipeline",
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
