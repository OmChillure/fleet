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
import unittest.mock
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


def _run_docker_build_if_needed(
    *,
    tree_id: str,
    exists: bool,
    label: str | None,
    prelude: str,
    repo_var: str,
    force: bool = False,
    env_sha: str | None = None,
) -> tuple[subprocess.CompletedProcess[str], str]:
    """Product docker_build_if_needed against mock docker. One temp dir per call."""
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
            f"{repo_var}={shlex.quote(str(repo))}\n"
            f"FORCE_REBUILD={1 if force else 0}\n"
            f"LOG={shlex.quote(str(state / 'log'))}\n"
            f"{prelude}\n"
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


SCRIPT_PATHS = (BOOTSTRAP, EVERY, FLEET_DEV, HUMAN, INSTALL, FULL_SUITE)


class FailClosedContractTests(unittest.TestCase):
    def test_only_install_script_installs_dx(self):
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
        m = re.search(
            r"(?m)^if ! need dx; then\n(?:(?!^fi$).*\n)*?fi$",
            text,
        )
        self.assertIsNotNone(
            m,
            "bootstrap must `if ! need dx; then` (inverted polarity is not fail-closed)",
        )
        block = m.group(0)
        self.assertRegex(
            block,
            r"(?m)^  exit 1$",
            "missing dx must exit 1 inside the then-block, not a later playwright exit",
        )
        self.assertIn("install-full-suite-deps.sh", block)

    def test_every_test_requires_tools_and_host_dx_build(self):
        text = EVERY.read_text(encoding="utf-8")
        m = re.search(
            r'(?m)^if ! python3 "\$\{ROOT\}/tests/browser/stack_identity\.py" --tools; then\n(?:(?!^fi$).*\n)*?fi$',
            text,
        )
        self.assertIsNotNone(
            m,
            "every-test must `if ! … stack_identity.py --tools; then` "
            "(inverted polarity is not fail-closed)",
        )
        block = m.group(0)
        self.assertIn("EVERY-TEST FAILED", block)
        self.assertRegex(
            block,
            r"(?m)^  exit 1$",
            "missing tools must exit 1 inside the then-block, not a later summary exit",
        )
        first_step = re.search(r"(?m)^run_step ", text)
        self.assertIsNotNone(first_step, "every-test must invoke run_step after the tools gate")
        self.assertLess(
            m.end(),
            first_step.start(),
            "tools gate must exit 1 before any run_step",
        )
        invoke = re.search(r"(?m)^run_step host-dx-build host_dx_build$", text)
        self.assertIsNotNone(
            invoke,
            "host_dx_build must be invoked as a run_step, not a leftover unused function",
        )
        host = _bash_fn(text, "host_dx_build")
        self.assertIn("dx build", host)
        self.assertIn("fleet-git-sha.txt", host)

    def test_fleet_dev_does_not_skip_ui_without_dx(self):
        text = FLEET_DEV.read_text(encoding="utf-8")
        start_ui = _bash_fn(text, "start_ui")
        skip = re.search(
            r'(?m)^  if \[\[ "\$\{FLEET_SKIP_UI:-0\}" == "1" \]\]; then\n(?:(?!^  fi$).*\n)*?  fi$',
            start_ui,
        )
        self.assertIsNotNone(
            skip,
            "start_ui must `if FLEET_SKIP_UI=1; then` return "
            "(missing dx is not an allowed skip)",
        )
        self.assertRegex(
            skip.group(0),
            r"(?m)^    return 0$",
            "FLEET_SKIP_UI=1 is the operator opt-out; it must return, not fall through",
        )
        after = start_ui[skip.end() :]
        self.assertRegex(
            after,
            r"\A\n  need_cmd dx \|\| exit 1\n",
            "start_ui after the FLEET_SKIP_UI=1 return must `need_cmd dx || exit 1`",
        )
        start = _bash_fn(text, "cmd_start")
        gate = re.search(
            r'(?m)^  if \[\[ "\$\{FLEET_SKIP_UI:-0\}" != "1" \]\]; then\n(?:(?!^  fi$).*\n)*?  fi$',
            start,
        )
        self.assertIsNotNone(
            gate,
            "cmd_start must `if FLEET_SKIP_UI != 1; then need_cmd dx` "
            "(inverted polarity skips UI when dx is missing)",
        )
        self.assertRegex(
            gate.group(0),
            r"(?m)^    need_cmd dx \|\| exit 1$",
            "cmd_start must exit 1 when dx is missing unless FLEET_SKIP_UI=1",
        )

    def test_fleet_dev_remaps_when_kind_owns_ports(self):
        """Kind extraPortMappings have no host pid; remap host stack to 18080/13030."""
        opened = self._run_port_is_open(8080, 8080)
        self.assertEqual(opened.returncode, 0, opened.stdout + opened.stderr)
        ran, urls = self._run_choose_host_ports(8080, 3030)
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        self.assertIn("FLEET_API_PORT=18080\n", urls)
        self.assertIn("FLEET_UI_PORT=13030\n", urls)
        self.assertIn("FLEET_API_URL=http://127.0.0.1:18080\n", urls)
        self.assertIn("FLEET_UI_URL=http://localhost:13030\n", urls)
        self.assertIn("FLEET_API_URL=http://127.0.0.1:18080", ran.stdout)

    def test_port_is_open_true_when_lsof_empty_and_ss_listens(self):
        """Kind extraPortMappings: lsof/fuser empty, ss -ltn still sees :8080."""
        ran = self._run_port_is_open(8080, 8080)
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        missed = self._run_port_is_open(8080)
        self.assertNotEqual(
            missed.returncode,
            0,
            "empty lsof + ss without :8080 must not report open",
        )

    def test_choose_host_ports_writes_18080_13030_when_8080_identity_fails(self):
        ran, urls = self._run_choose_host_ports(8080, 3030)
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        self.assertIn("FLEET_API_PORT=18080\n", urls)
        self.assertIn("FLEET_UI_PORT=13030\n", urls)
        self.assertIn("PORT=18080\n", urls)
        self.assertIn("FLEET_API_URL=http://127.0.0.1:18080\n", urls)
        self.assertIn("FLEET_UI_URL=http://localhost:13030\n", urls)
        self.assertIn("FLEET_API_URL=http://127.0.0.1:18080", ran.stdout)
        self.assertIn("FLEET_UI_URL=http://localhost:13030", ran.stdout)

    def test_choose_host_ports_explicit_FLEET_API_PORT_wins(self):
        ran, urls = self._run_choose_host_ports(
            8080,
            3030,
            fleet_api_port="19080",
            fleet_ui_port="14030",
        )
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        self.assertIn("FLEET_API_PORT=19080\n", urls)
        self.assertIn("FLEET_UI_PORT=14030\n", urls)
        self.assertIn("FLEET_API_URL=http://127.0.0.1:19080\n", urls)
        self.assertIn("FLEET_UI_URL=http://localhost:14030\n", urls)
        self.assertNotIn("18080", urls)
        self.assertNotIn("13030", urls)

    def _write_exec(self, path: Path, body: str) -> None:
        path.write_text(body, encoding="utf-8")
        path.chmod(0o755)

    def _kind_no_pid_bins(self, bin_dir: Path, *listen: int) -> None:
        bin_dir.mkdir(parents=True, exist_ok=True)
        self._write_exec(bin_dir / "lsof", "#!/usr/bin/env bash\nexit 0\n")
        self._write_exec(bin_dir / "fuser", "#!/usr/bin/env bash\nexit 0\n")
        self._write_exec(bin_dir / "curl", "#!/usr/bin/env bash\nexit 1\n")
        lines = ["State Recv-Q Send-Q Local Address:Port Peer Address:Port"]
        for port in listen:
            lines.append(f"LISTEN 0 128 0.0.0.0:{port} 0.0.0.0:*")
        printed = "\n".join(lines) + "\n"
        self._write_exec(
            bin_dir / "ss",
            "#!/usr/bin/env bash\n"
            "set -euo pipefail\n"
            f"printf '%s' {shlex.quote(printed)}\n",
        )

    def _run_port_is_open(
        self, port: int, *listen: int
    ) -> subprocess.CompletedProcess[str]:
        text = FLEET_DEV.read_text(encoding="utf-8")
        fns = "\n".join((_bash_fn(text, "port_pids"), _bash_fn(text, "port_is_open")))
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            bin_dir = root / "bin"
            self._kind_no_pid_bins(bin_dir, *listen)
            (root / "run.sh").write_text(
                "#!/usr/bin/env bash\n"
                "set -euo pipefail\n"
                f"{fns}\n"
                f"port_is_open {int(port)}\n",
                encoding="utf-8",
            )
            env = os.environ.copy()
            env["PATH"] = f"{bin_dir}{os.pathsep}{env.get('PATH', '')}"
            return subprocess.run(
                ["bash", str(root / "run.sh")],
                cwd=str(root),
                capture_output=True,
                text=True,
                env=env,
                check=False,
            )

    def _run_choose_host_ports(
        self,
        *listen: int,
        fleet_api_port: str | None = None,
        fleet_ui_port: str | None = None,
    ) -> tuple[subprocess.CompletedProcess[str], str]:
        text = FLEET_DEV.read_text(encoding="utf-8")
        fns = "\n".join(
            (
                _bash_fn(text, "port_pids"),
                _bash_fn(text, "port_is_open"),
                'warn() { printf "%s\\n" "$*" >&2; }',
                _bash_fn(text, "choose_host_ports"),
            )
        )
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            run_dir = root / "run"
            bin_dir = root / "bin"
            self._kind_no_pid_bins(bin_dir, *listen)
            (root / "scripts").mkdir()
            self._write_exec(
                root / "scripts" / "tree-id.sh",
                "#!/usr/bin/env bash\nprintf '%s\\n' testhosttreeid\n",
            )
            ident = root / "tests" / "browser"
            ident.mkdir(parents=True)
            (ident / "stack_identity.py").write_text(
                "import sys\nraise SystemExit(1)\n",
                encoding="utf-8",
            )
            (root / "run.sh").write_text(
                "#!/usr/bin/env bash\n"
                "set -euo pipefail\n"
                f"ROOT={shlex.quote(str(root))}\n"
                f"RUN_DIR={shlex.quote(str(run_dir))}\n"
                'URLS_FILE="${RUN_DIR}/host-urls.env"\n'
                "API_PORT=8080\n"
                "UI_PORT=3030\n"
                "HOST_API_FALLBACK=18080\n"
                "HOST_UI_FALLBACK=13030\n"
                f"{fns}\n"
                "choose_host_ports\n"
                'printf "API_PORT=%s\\n" "$API_PORT"\n'
                'printf "UI_PORT=%s\\n" "$UI_PORT"\n'
                'printf "FLEET_API_URL=%s\\n" "$FLEET_API_URL"\n'
                'printf "FLEET_UI_URL=%s\\n" "$FLEET_UI_URL"\n',
                encoding="utf-8",
            )
            env = os.environ.copy()
            env["PATH"] = f"{bin_dir}{os.pathsep}{env.get('PATH', '')}"
            for key in (
                "FLEET_API_PORT",
                "FLEET_UI_PORT",
                "FLEET_HOST_API_FALLBACK",
                "FLEET_HOST_UI_FALLBACK",
            ):
                env.pop(key, None)
            if fleet_api_port is not None:
                env["FLEET_API_PORT"] = fleet_api_port
            if fleet_ui_port is not None:
                env["FLEET_UI_PORT"] = fleet_ui_port
            ran = subprocess.run(
                ["bash", str(root / "run.sh")],
                cwd=str(root),
                capture_output=True,
                text=True,
                env=env,
                check=False,
            )
            urls_path = run_dir / "host-urls.env"
            urls = urls_path.read_text(encoding="utf-8") if urls_path.is_file() else ""
            return ran, urls

    def test_identity_cannot_be_satisfied_by_kubectl_env_or_foreign_allow(self):
        """Host/plane bake, infra YAML, IDENT env, EVERY/FULL_SUITE if-window."""
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

        ident = IDENT.read_text(encoding="utf-8")
        self.assertNotIn('os.environ.get("FLEET_GIT_SHA")', ident)
        self.assertNotIn("os.environ['FLEET_GIT_SHA']", ident)
        self.assertNotIn('os.getenv("FLEET_GIT_SHA")', ident)

        for path in (EVERY, FULL_SUITE):
            text = path.read_text(encoding="utf-8")
            window = re.search(
                r'(?m)^if \[\[ "\$\{FLEET_ALLOW_FOREIGN_STACK:-\}" == "1" \]\]; then\n(?:(?!^fi$).*\n)*?^fi$',
                text,
            )
            self.assertIsNotNone(
                window,
                f"{path.name} must `if FLEET_ALLOW_FOREIGN_STACK=1; then` (inverted polarity is not fail-closed)",
            )
            self.assertRegex(
                window.group(0),
                r"(?m)^  exit 1$",
                f"{path.name} must exit 1 inside the then-block",
            )
            self.assertRegex(
                text[window.end() :],
                r"\A\nunset FLEET_ALLOW_FOREIGN_STACK",
                f"{path.name} must unset FLEET_ALLOW_FOREIGN_STACK after the if-window",
            )

        for yml in (REPO / "infra").rglob("*.yaml"):
            text = yml.read_text(encoding="utf-8")
            self.assertNotIn(
                "FLEET_GIT_SHA",
                text,
                f"{yml.relative_to(REPO)} must not inject runtime FLEET_GIT_SHA",
            )

    def test_list_hosts_plane_git_sha_equals_health_git_sha(self):
        """GET worker-hosts plane_git_sha is the same compile-time bake as /health git_sha."""
        lib = SERVER_LIB.read_text(encoding="utf-8")
        health = re.search(
            r"let git_sha = (option_env!\(\"FLEET_GIT_SHA\"\)[^;]+);",
            lib,
        )
        self.assertIsNotNone(health, "health git_sha must be compile-time option_env!")
        self.assertIn('"git_sha": git_sha', lib)

        plane = (
            REPO / "crates" / "fleet-server" / "src" / "worker_hosts.rs"
        ).read_text(encoding="utf-8")
        plane_fn = re.search(r"fn plane_git_sha\(\)[^{]*\{([^}]+)\}", plane)
        self.assertIsNotNone(plane_fn, "plane_git_sha must exist")
        self.assertEqual(
            re.sub(r"\s+", "", health.group(1)),
            re.sub(r"\s+", "", plane_fn.group(1)),
            "list plane_git_sha must be the same compile-time bake as /health git_sha",
        )
        self.assertIn('"plane_git_sha": plane_git_sha()', plane)
        self.assertNotIn('std::env::var("FLEET_GIT_SHA")', plane)
        self.assertNotIn('env::var("FLEET_GIT_SHA")', plane)

        pkgs = (
            REPO / "crates" / "fleet-server" / "src" / "package_versions.rs"
        ).read_text(encoding="utf-8")
        self.assertIn('option_env!("FLEET_GIT_SHA")', pkgs)
        self.assertNotIn('std::env::var("FLEET_GIT_SHA")', pkgs)
        self.assertNotIn('env::var("FLEET_GIT_SHA")', pkgs)

    def test_fleet_host_build_rs_reruns_on_fleet_git_sha(self):
        build_rs = (REPO / "crates" / "fleet-host" / "build.rs").read_text(
            encoding="utf-8"
        )
        self.assertIn(
            'println!("cargo:rerun-if-env-changed=FLEET_GIT_SHA");',
            build_rs,
        )

    def test_live_identity_fails_on_mismatch_despite_kubectl_env_and_foreign_allow(self):
        """/health.git_sha != tree-id.sh is foreign; FLEET_GIT_SHA / ALLOW are not a green path."""
        spec = importlib.util.spec_from_file_location("stack_identity", IDENT)
        self.assertIsNotNone(spec)
        self.assertIsNotNone(spec.loader)
        ident = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(ident)

        foreign = "a" * 40
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

                with self.assertRaises(ident.ForeignStackError) as ctx:
                    ident.require_this_tree(root, url, require_ui=False)
                err = str(ctx.exception)
                self.assertIn(foreign, err)
                self.assertIn(want, err)
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

    def test_live_identity_fails_when_ui_sha_mismatches_despite_matching_api_and_foreign_allow(self):
        """Matching /health.git_sha still fails when UI /fleet-git-sha.txt differs."""
        spec = importlib.util.spec_from_file_location("stack_identity", IDENT)
        self.assertIsNotNone(spec)
        self.assertIsNotNone(spec.loader)
        ident = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(ident)

        want = "b" * 40
        foreign = "a" * 40
        state = {"sha": want, "ui_sha": foreign}

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                path = self.path.split("?", 1)[0]
                if path == "/health":
                    payload = json.dumps(
                        {
                            "status": "ok",
                            "service": "fleet-server",
                            "db": "up",
                            "git_sha": state["sha"],
                            "apis": ["auth", "workspaces", "chat"],
                        }
                    ).encode()
                    ctype = "application/json"
                elif path == "/fleet-git-sha.txt":
                    payload = state["ui_sha"].encode()
                    ctype = "text/plain"
                else:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Type", ctype)
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def log_message(self, *args):
                return

        httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=httpd.serve_forever, daemon=True).start()
        url = f"http://127.0.0.1:{httpd.server_address[1]}"
        try:
            with tempfile.TemporaryDirectory() as td:
                root = Path(td)
                (root / "scripts").mkdir()
                (root / "scripts" / "tree-id.sh").write_text(
                    f"#!/usr/bin/env bash\nprintf '%s\\n' '{want}'\n",
                    encoding="utf-8",
                )
                with unittest.mock.patch.dict(os.environ, {"FLEET_UI_URL": url}):
                    with self.assertRaises(ident.ForeignStackError) as ctx:
                        ident.require_this_tree(root, url, require_ui=True)
                err = str(ctx.exception)
                self.assertIn(foreign, err)
                self.assertIn(want, err)
                self.assertIn("/fleet-git-sha.txt", err)

                env = os.environ.copy()
                env["FLEET_API_URL"] = url
                env["FLEET_UI_URL"] = url
                env["FLEET_GIT_SHA"] = foreign
                env["FLEET_ALLOW_FOREIGN_STACK"] = "1"
                ran = subprocess.run(
                    [sys.executable, str(IDENT), "--repo", str(root)],
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
                self.assertIn("/fleet-git-sha.txt", out)
                self.assertNotIn("THIS_TREE", out)
                self.assertNotIn("WARN:", out)
        finally:
            httpd.shutdown()
            httpd.server_close()

    def test_dockerfile_bakes_sha_on_consuming_compile(self):
        """Dep fetch/warm must precede ARG. Sha-consuming cargo must see ARG/ENV.

        Tree-id is identity, not the crates.io cache key. A dirty suffix must
        not force `Downloading crates` / kube-sqlx-zenoh rebuild.
        """
        text = DOCKERFILE.read_text(encoding="utf-8")
        builder, sep, _runtime = text.partition("\nFROM debian")
        self.assertTrue(sep, "Dockerfile must have a debian runtime stage")
        self.assertIn("cargo fetch --locked", builder)
        first_fetch = builder.find("cargo fetch")
        first_build = builder.find("cargo build")
        arg_at = builder.rfind("ARG FLEET_GIT_SHA")
        last_cargo = builder.rfind("cargo build")
        self.assertGreaterEqual(first_fetch, 0)
        self.assertGreaterEqual(first_build, 0)
        self.assertGreaterEqual(arg_at, 0, "builder must declare ARG FLEET_GIT_SHA")
        self.assertLess(
            first_fetch,
            arg_at,
            "dep cargo fetch must precede ARG so tree-id is not the cache key",
        )
        self.assertLess(
            first_build,
            arg_at,
            "first cargo build must precede ARG so a dirty suffix does not rebuild kube/sqlx/zenoh",
        )
        self.assertLess(
            arg_at,
            last_cargo,
            "sha-consuming cargo build must follow ARG/ENV (compile-time bake)",
        )
        self.assertIn("ENV FLEET_GIT_SHA=${FLEET_GIT_SHA}", builder)
        post_arg = builder[arg_at:]
        post_cargo = re.search(r"cargo build(?:[^\n]*\\\n)*[^\n]*", post_arg)
        self.assertIsNotNone(post_cargo, "sha-consuming cargo build must follow ARG")
        post_cmd = post_cargo.group(0).replace("\\\n", " ")
        self.assertIn("-p fleet-server", post_cmd)
        self.assertIn("-p fleet-host", post_cmd)
        # Runtime must not inject identity (kubectl set env is not a green path).
        runtime = text[text.rfind("\nFROM ") :]
        self.assertNotRegex(
            runtime,
            r"(?m)^ENV FLEET_GIT_SHA=",
            "runtime must not inject identity (kubectl set env is not a green path)",
        )

    def test_dockerfile_warm_cargo_build_precedes_fleet_git_sha_arg(self):
        """Warm cargo build (not only fetch) must precede ARG FLEET_GIT_SHA."""
        text = DOCKERFILE.read_text(encoding="utf-8")
        builder, sep, _runtime = text.partition("\nFROM debian")
        self.assertTrue(sep, "Dockerfile must have a debian runtime stage")
        first_build = builder.find("cargo build")
        arg_at = builder.find("ARG FLEET_GIT_SHA")
        self.assertGreaterEqual(first_build, 0, "builder must cargo build")
        self.assertGreaterEqual(arg_at, 0, "builder must declare ARG FLEET_GIT_SHA")
        self.assertLess(
            first_build,
            arg_at,
            "first cargo build must precede ARG so a dirty suffix does not rebuild kube/sqlx/zenoh",
        )

    def test_dockerfile_post_arg_cargo_includes_fleet_server_and_host(self):
        """The cargo build after ARG must -p fleet-server -p fleet-host."""
        text = DOCKERFILE.read_text(encoding="utf-8")
        builder, sep, _runtime = text.partition("\nFROM debian")
        self.assertTrue(sep, "Dockerfile must have a debian runtime stage")
        arg_at = builder.find("ARG FLEET_GIT_SHA")
        self.assertGreaterEqual(arg_at, 0, "builder must declare ARG FLEET_GIT_SHA")
        post_arg = builder[arg_at:]
        post_cargo = re.search(r"cargo build(?:[^\n]*\\\n)*[^\n]*", post_arg)
        self.assertIsNotNone(
            post_cargo,
            "sha-consuming cargo build must follow ARG (a dummy cargo stays empty option_env!)",
        )
        post_cmd = post_cargo.group(0).replace("\\\n", " ")
        self.assertIn("-p fleet-server", post_cmd)
        self.assertIn("-p fleet-host", post_cmd)

    def test_compose_requires_sha_build_arg(self):
        """api/ui/host pass FLEET_GIT_SHA with empty default; :? locks `up postgres`."""
        text = (REPO / "docker-compose.yml").read_text(encoding="utf-8")
        # Compose interpolates the whole file even for `up postgres`.
        self.assertNotIn("${FLEET_GIT_SHA:?", text)
        self.assertNotIn("${FLEET_GIT_SHA?", text)
        sha_arg = "FLEET_GIT_SHA: ${FLEET_GIT_SHA:-}"
        for name in ("api", "ui", "host"):
            svc = re.search(rf"(?ms)^  {name}:\n(.*?)(?=^  [a-z]|^[a-z]|\Z)", text)
            self.assertIsNotNone(svc, f"{name} service missing")
            build = re.search(r"(?ms)^    build:\n((?:      .+\n)+)", svc.group(1))
            self.assertIsNotNone(build, f"{name} missing build:")
            self.assertIn("args:", build.group(1), f"{name} missing build.args")
            self.assertIn(sha_arg, build.group(1), f"{name} must pass {sha_arg}")

    def test_install_reassigns_fleet_git_sha_from_tree_id_after_dotenv(self):
        """Leftover .env SHA must not interpolate into compose."""
        text = INSTALL.read_text(encoding="utf-8")
        src_at = text.find('. "${ROOT_ENV}"')
        self.assertGreaterEqual(src_at, 0, "install must source repo-root .env")
        after = text[src_at:]
        self.assertRegex(
            after,
            r'FLEET_GIT_SHA="\$\(bash "\$\{REPO\}/scripts/tree-id\.sh"\)"',
            "after sourcing .env, FLEET_GIT_SHA must be reset from tree-id.sh",
        )

    def test_ui_image_embeds_sha_file_and_label(self):
        """UI image must write /fleet-git-sha.txt; nginx must 404 if missing."""
        text = DOCKERFILE_UI.read_text(encoding="utf-8")
        self.assertIn(
            """printf '%s\\n' "${FLEET_GIT_SHA}" > crates/fleet-ui/public/fleet-git-sha.txt""",
            text,
        )
        self.assertIn(
            """printf '%s\\n' "${FLEET_GIT_SHA}" > /out/fleet-git-sha.txt""",
            text,
        )
        nginx = (REPO / "deploy" / "nginx-ui.conf").read_text(encoding="utf-8")
        self.assertIn(
            "location = /fleet-git-sha.txt {\n        try_files $uri =404;",
            nginx,
        )

    def _assert_local_skip_only_on_label_eq_tree_id(
        self, prelude: str, repo_var: str
    ) -> None:
        tree = "0123456789abcdef0123456789abcdef01234567"
        dirty = tree + "-aabbccddeeff"
        stale = "ffffffffffffffffffffffffffffffffffffffff"

        def run_build(
            *,
            tree_id: str,
            exists: bool,
            label: str | None,
            force: bool = False,
            env_sha: str | None = None,
        ) -> tuple[subprocess.CompletedProcess[str], str]:
            return _run_docker_build_if_needed(
                tree_id=tree_id,
                exists=exists,
                label=label,
                prelude=prelude,
                repo_var=repo_var,
                force=force,
                env_sha=env_sha,
            )

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

    def test_local_images_skip_rebuild_only_when_label_matches_tree_id(self):
        """Install :local skips only when inspect fleet.git_sha == tree-id.sh.

        Matching label may skip. Existence of the tag, leftover FLEET_GIT_SHA,
        or `git rev-parse HEAD` (no dirty suffix) is not enough.
        """
        install = INSTALL.read_text(encoding="utf-8")
        prelude = "\n".join(
            (
                _bash_fn(install, "have_image"),
                _bash_fn(install, "docker_build_if_needed"),
            )
        )
        self._assert_local_skip_only_on_label_eq_tree_id(prelude, "REPO")

    def test_every_docker_build_if_needed_skips_only_when_inspect_label_equals_tree_id(
        self,
    ):
        """EVERY docker_build_if_needed skips only when inspect label == tree-id.sh.

        prepare_kind exports leftover FLEET_GIT_SHA; skip must still use tree-id.sh
        and must rebuild on inspect-success without a matching fleet.git_sha.
        """
        every = EVERY.read_text(encoding="utf-8")
        prelude = "\n".join(
            (
                'log() { printf \'%s\\n\' "$*"; }',
                _bash_fn(every, "image_label_sha"),
                _bash_fn(every, "docker_build_if_needed"),
            )
        )
        self._assert_local_skip_only_on_label_eq_tree_id(prelude, "ROOT")

    def test_tree_id_lock_requires_rebuild_after_changes(self):
        """Public tree-id.sh --self-test: clean HEAD, dirty suffix, restore, ignores."""
        ran = subprocess.run(
            ["bash", str(REPO / "scripts" / "tree-id.sh"), "--self-test"],
            cwd=str(REPO),
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        self.assertIn("tree-id self-test ok", ran.stdout)

    def _clean_tree_id_worktree(
        self, td: str, extra_tracked: dict[str, str] | None = None
    ) -> tuple[Path, str]:
        """Commit product tree-id.sh (+ extras) in *td*. Returns (root, clean HEAD)."""
        root = Path(td)
        (root / "scripts").mkdir()
        script = root / "scripts" / "tree-id.sh"
        script.write_text(
            (REPO / "scripts" / "tree-id.sh").read_text(encoding="utf-8"),
            encoding="utf-8",
        )
        script.chmod(0o755)
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
        add = ["scripts/tree-id.sh", "tracked.txt"]
        for rel, body in (extra_tracked or {}).items():
            path = root / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(body, encoding="utf-8")
            add.append(rel)
        subprocess.run(["git", "add", *add], cwd=td, check=True)
        subprocess.run(["git", "commit", "-q", "-m", "init"], cwd=td, check=True)
        clean = subprocess.check_output(
            ["bash", str(script)], text=True, cwd=td
        ).strip()
        self.assertRegex(clean, r"^[0-9a-f]{40}$", clean)
        return root, clean

    def test_tree_id_human_test_scope_md_edit_keeps_clean_id(self):
        """PATHSPEC :!HUMAN_TEST_SCOPE.md — Film review must not dirty HEAD."""
        with tempfile.TemporaryDirectory() as td:
            root, clean = self._clean_tree_id_worktree(td)
            (root / "HUMAN_TEST_SCOPE.md").write_text(
                "# Film review\nlooks good\n",
                encoding="utf-8",
            )
            after = subprocess.check_output(
                ["bash", str(root / "scripts" / "tree-id.sh")],
                text=True,
                cwd=td,
            ).strip()
            self.assertEqual(after, clean)
            self.assertRegex(after, r"^[0-9a-f]{40}$", after)

    def test_tree_id_generated_fleet_git_sha_txt_does_not_dirty_id(self):
        """Baked UI sha file is gitignored; writing it must stay clean HEAD."""
        with tempfile.TemporaryDirectory() as td:
            root, clean = self._clean_tree_id_worktree(
                td,
                extra_tracked={
                    ".gitignore": (REPO / ".gitignore").read_text(encoding="utf-8"),
                },
            )
            baked = root / "crates" / "fleet-ui" / "public" / "fleet-git-sha.txt"
            baked.parent.mkdir(parents=True, exist_ok=True)
            baked.write_text(clean + "\n", encoding="utf-8")
            after = subprocess.check_output(
                ["bash", str(root / "scripts" / "tree-id.sh")],
                text=True,
                cwd=td,
            ).strip()
            self.assertEqual(after, clean)
            self.assertRegex(after, r"^[0-9a-f]{40}$", after)

    def _run_stack_identity_tools(
        self, on_path: tuple[str, ...]
    ) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as td:
            bin_dir = Path(td)
            for name in on_path:
                self._write_exec(bin_dir / name, "#!/usr/bin/env bash\nexit 0\n")
            env = os.environ.copy()
            env["PATH"] = str(bin_dir)
            return subprocess.run(
                [sys.executable, str(IDENT), "--tools"],
                cwd=str(REPO),
                capture_output=True,
                text=True,
                env=env,
                check=False,
            )

    def test_tools_gate_fails_when_dx_absent(self):
        """--tools fails closed when only host dx is missing."""
        r = self._run_stack_identity_tools(
            ("git", "cargo", "rustc", "python3", "docker")
        )
        self.assertNotEqual(r.returncode, 0, r.stdout + r.stderr)
        err = r.stderr + r.stdout
        self.assertIn("MISSING_TOOLS", err)
        listed = re.search(r"MISSING_TOOLS:\s*([^.]+)", err)
        self.assertIsNotNone(listed, err)
        self.assertEqual(
            [n.strip() for n in listed.group(1).split(",") if n.strip()],
            ["dx"],
            err,
        )
        self.assertNotIn("TOOLS_OK", err)
        self.assertNotIn("WARN:", err)

    def test_tools_gate_ok_when_all_required_on_path(self):
        """--tools prints TOOLS_OK only when every required tool is on PATH."""
        r = self._run_stack_identity_tools(
            ("git", "cargo", "rustc", "python3", "docker", "dx")
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        out = r.stdout + r.stderr
        self.assertIn("TOOLS_OK", out)
        self.assertNotIn("MISSING_TOOLS", out)
        self.assertNotIn("WARN:", out)

    def test_human_test_launch_requires_ui_identity(self):
        """cmd_launch runs fleet-dev start then stack_identity.py (not --api-only)."""
        text = HUMAN.read_text(encoding="utf-8")
        self.assertRegex(
            text,
            r'(?m)^DEV="\$\{ROOT\}/scripts/fleet-dev\.sh"$',
            "DEV must be scripts/fleet-dev.sh",
        )
        launch = _bash_fn(text, "cmd_launch")
        start = re.search(r'(?m)^  "\$\{DEV\}" start$', launch)
        self.assertIsNotNone(start, "cmd_launch must run fleet-dev.sh start")
        ident = re.search(r"(?m)^  require_this_tree \|\| \{", launch)
        self.assertIsNotNone(
            ident, "cmd_launch must require_this_tree after fleet-dev start"
        )
        self.assertLess(
            start.start(),
            ident.start(),
            "fleet-dev start must precede require_this_tree",
        )
        self.assertRegex(
            launch[ident.start() :],
            r"\A  require_this_tree \|\| \{\n(?:.*\n)*?    exit 1\n  \}",
            "failed this-tree identity after launch must exit 1",
        )
        self.assertNotRegex(
            launch,
            r"stack_identity\.py\b[^\n]*--api-only",
            "cmd_launch must not invoke stack_identity.py --api-only",
        )
        invoke = re.search(
            r'(?m)^\s*python3 "\$\{ROOT\}/tests/browser/stack_identity\.py"(.*)$',
            _bash_fn(text, "require_this_tree"),
        )
        self.assertIsNotNone(
            invoke, "require_this_tree must invoke tests/browser/stack_identity.py"
        )
        self.assertNotIn(
            "--api-only",
            invoke.group(1),
            "require_this_tree must not pass --api-only (UI identity)",
        )

    def test_human_test_require_this_tree_is_not_api_only(self):
        """require_this_tree is UI identity; cmd_guide/cmd_record call it first."""
        text = HUMAN.read_text(encoding="utf-8")
        invoke = re.search(
            r'(?m)^\s*python3 "\$\{ROOT\}/tests/browser/stack_identity\.py"(.*)$',
            _bash_fn(text, "require_this_tree"),
        )
        self.assertIsNotNone(
            invoke, "require_this_tree must invoke tests/browser/stack_identity.py"
        )
        self.assertNotIn(
            "--api-only",
            invoke.group(1),
            "require_this_tree must not pass --api-only (UI identity)",
        )
        guide = _bash_fn(text, "cmd_guide")
        gate = re.search(
            r'(?m)^  if ! require_this_tree; then\n(?:(?!^  fi$).*\n)*?  fi$',
            guide,
        )
        self.assertIsNotNone(
            gate,
            "cmd_guide must `if ! require_this_tree; then` "
            "(inverted polarity is not fail-closed)",
        )
        self.assertRegex(
            gate.group(0),
            r"(?m)^    exit 1$",
            "cmd_guide must exit 1 when this-tree identity fails",
        )
        first_pause = re.search(r"(?m)^  pause_step ", guide)
        self.assertIsNotNone(first_pause, "cmd_guide must walk pause_step")
        self.assertLess(
            gate.end(),
            first_pause.start(),
            "cmd_guide must require_this_tree before walking",
        )
        self.assertRegex(
            text,
            r'(?m)^REC="\$\{ROOT\}/tests/browser/record_demo\.py"$',
            "REC must be tests/browser/record_demo.py",
        )
        record = _bash_fn(text, "cmd_record")
        rec_ident = re.search(r"\brequire_this_tree\b", record)
        self.assertIsNotNone(rec_ident, "cmd_record must call require_this_tree")
        rec_film = re.search(r'(?m)^  exec python3 "\$\{REC\}"', record)
        self.assertIsNotNone(rec_film, "cmd_record must exec record_demo.py")
        self.assertLess(
            rec_ident.start(),
            rec_film.start(),
            "cmd_record must require_this_tree before filming",
        )

    def test_human_test_does_not_export_fleet_skip_ui(self):
        """human-test.sh must not assign or export FLEET_SKIP_UI."""
        text = HUMAN.read_text(encoding="utf-8")
        lines: list[str] = []
        for raw in text.replace("\r\n", "\n").split("\n"):
            stripped = raw.split("#", 1)[0]
            if stripped.strip():
                lines.append(stripped)
        active = "\n".join(lines)
        active = re.sub(r'"([^"\\]|\\.)*"', '""', active)
        active = re.sub(r"'[^']*'", "''", active)
        self.assertNotRegex(
            active,
            r"(?<![\w])FLEET_SKIP_UI=",
            "human-test.sh must not assign FLEET_SKIP_UI",
        )
        self.assertNotRegex(
            active,
            r"(?<![\w])export\s+FLEET_SKIP_UI\b",
            "human-test.sh must not export FLEET_SKIP_UI",
        )

    def test_bootstrap_postgres_wait_is_fail_closed(self):
        text = BOOTSTRAP.read_text(encoding="utf-8")
        wait = re.search(
            r"(?m)^if ! pg_isready[^\n]*-p 5433[^\n]*; then\n(?:(?!^fi$).*\n)*?fi$",
            text,
        )
        self.assertIsNotNone(
            wait,
            "bootstrap must `if ! pg_isready … -p 5433; then` "
            "(inverted polarity is not fail-closed)",
        )
        block = wait.group(0)
        compose = re.search(r"\bdocker compose up -d postgres\b", block)
        self.assertIsNotNone(
            compose, "not-ready must `docker compose up -d postgres`"
        )
        self.assertRegex(
            block[compose.end() :],
            r"\bpg_isready\b[^\n]*-p 5433",
            "after compose up, the wait loop must retry pg_isready on :5433",
        )
        fail = re.search(
            r'(?m)^  if \[\[ "\$\{[A-Za-z_][A-Za-z0-9_]*\}" -ne 1 \]\]; then\n'
            r"(?:(?!^  fi$).*\n)*?  fi$",
            block,
        )
        self.assertIsNotNone(
            fail,
            "not-ready must be `if [[ \"${ready}\" -ne 1 ]]` (eq is inverted)",
        )
        self.assertRegex(
            fail.group(0),
            r"(?m)^    exit 1$",
            "not-ready must exit 1 inside the then-block, not || true",
        )

    def test_bootstrap_rewrites_database_url_to_compose_5433(self):
        text = BOOTSTRAP.read_text(encoding="utf-8")
        rewrite = re.search(
            r"(?m)^if grep -qE '\^DATABASE_URL=' \"\$\{ROOT\}/\.env\"; then\n"
            r"(?:(?!^fi$).*\n)*?fi$",
            text,
        )
        self.assertIsNotNone(
            rewrite,
            "bootstrap must rewrite existing DATABASE_URL or append if missing",
        )
        block = rewrite.group(0)
        else_m = re.search(r"(?m)^else$", block)
        self.assertIsNotNone(
            else_m, "missing DATABASE_URL must take an else-append path"
        )
        url = "postgres://fleet:fleet@127.0.0.1:5433/fleet"
        self.assertIn(
            url,
            block[: else_m.start()],
            "existing DATABASE_URL must be rewritten to compose :5433",
        )
        else_body = block[else_m.end() :]
        self.assertIn(
            url,
            else_body,
            "missing DATABASE_URL must be appended as compose :5433",
        )
        self.assertRegex(
            else_body,
            r">>",
            "missing DATABASE_URL must be appended, not only printed",
        )

    def test_install_sandbox_lab_postgres_wait_is_fail_closed(self):
        text = INSTALL.read_text(encoding="utf-8")
        lab = re.search(
            r'(?m)^if \[\[ "\$\{ENABLE_SANDBOX_LAB\}" == "1" \]\]; then\n'
            r"(?:(?!^fi$).*\n)*?fi$",
            text,
        )
        self.assertIsNotNone(
            lab,
            "install must wait for postgres inside ENABLE_SANDBOX_LAB=1",
        )
        block = lab.group(0)
        self.assertRegex(
            block,
            r"\bdc up -d postgres\b",
            "ENABLE_SANDBOX_LAB must start compose postgres",
        )
        self.assertRegex(
            block,
            r"\bpg_isready\b",
            "ENABLE_SANDBOX_LAB must wait with pg_isready",
        )
        fail = re.search(
            r'(?m)^  if \[\[ "\$\{[A-Za-z_][A-Za-z0-9_]*\}" -ne 1 \]\]; then\n'
            r"(?:(?!^  fi$).*\n)*?  fi$",
            block,
        )
        self.assertIsNotNone(
            fail,
            "not-ready must be `if [[ \"${pg_ok}\" -ne 1 ]]` (eq is inverted)",
        )
        self.assertRegex(
            fail.group(0),
            r"(?m)^    exit 1$",
            "not-ready must exit 1 inside the then-block, not || true",
        )

    def test_ui_dockerfile_sha_after_dx_install(self):
        """dx install + cargo fetch --locked before ARG; printf fleet-git-sha.txt after ARG."""
        text = DOCKERFILE_UI.read_text(encoding="utf-8")
        install_m = re.search(r"(?m)^(?!#)[^\n]*cargo install dioxus-cli", text)
        fetch_m = re.search(r"(?m)^(?!#)[^\n]*cargo fetch --locked", text)
        arg_at = text.find("ARG FLEET_GIT_SHA")
        write_public = text.find(
            """printf '%s\\n' "${FLEET_GIT_SHA}" > crates/fleet-ui/public/fleet-git-sha.txt"""
        )
        write_out = text.find(
            """printf '%s\\n' "${FLEET_GIT_SHA}" > /out/fleet-git-sha.txt"""
        )
        write_at = write_public if write_public >= 0 else write_out
        self.assertIsNotNone(install_m, "builder must cargo install dioxus-cli")
        self.assertIsNotNone(fetch_m, "builder must cargo fetch --locked")
        self.assertGreaterEqual(arg_at, 0, "builder must declare ARG FLEET_GIT_SHA")
        self.assertGreater(
            arg_at,
            install_m.start(),
            "builder ARG FLEET_GIT_SHA must come after cargo install dioxus-cli",
        )
        self.assertGreater(
            arg_at,
            fetch_m.start(),
            "cargo fetch --locked must precede ARG so a dirty tree-id does not refetch",
        )
        self.assertGreaterEqual(
            write_at,
            0,
            "must printf ${FLEET_GIT_SHA} to fleet-git-sha.txt or /out",
        )
        self.assertGreater(
            write_at,
            arg_at,
            "printf to fleet-git-sha.txt must follow ARG (UI identity is the static file)",
        )
        self.assertRegex(
            text[arg_at:],
            r"(?m)^(?!#)[^\n]*dx build",
            "dx build must run after ARG",
        )

    def test_operator_compiles_before_sha_label(self):
        """Operator cargo must exist and precede any builder ARG FLEET_GIT_SHA.

        Opposite of the API bake: operator does not consume option_env!, so a
        dirty tree-id must not be the cargo cache key. Runtime must not inject
        identity via ENV (kubectl set env is not a green path).
        """
        text = DOCKERFILE_OP.read_text(encoding="utf-8")
        last_from = text.rfind("\nFROM ")
        self.assertGreater(last_from, 0, "operator image must have a runtime stage")
        builder = text[:last_from]
        runtime = text[last_from:]
        cargo_at = builder.find("cargo build")
        self.assertGreaterEqual(cargo_at, 0, "builder must cargo build")
        arg_at = builder.find("ARG FLEET_GIT_SHA")
        if arg_at >= 0:
            self.assertLess(
                cargo_at,
                arg_at,
                "cargo build must precede any builder ARG FLEET_GIT_SHA "
                "so a dirty tree-id is not the cargo cache key",
            )
        self.assertNotIn(
            "ENV FLEET_GIT_SHA",
            runtime,
            "runtime must not inject identity (kubectl set env is not a green path)",
        )

    def test_operator_crate_does_not_read_fleet_git_sha(self):
        """fleet-operator src has no option_env!/env::var FLEET_GIT_SHA.

        Compile-before-ARG cannot leave an empty bake; health_sha siblings
        only lock server/host/plane. Identity is the inspectable LABEL only.
        """
        src = REPO / "crates" / "fleet-operator" / "src"
        paths = sorted(src.rglob("*.rs"))
        self.assertTrue(paths, "crates/fleet-operator/src must contain Rust sources")
        for path in paths:
            body = path.read_text(encoding="utf-8")
            rel = path.relative_to(REPO)
            self.assertNotIn(
                'option_env!("FLEET_GIT_SHA")',
                body,
                f"{rel} must not bake option_env! FLEET_GIT_SHA",
            )
            self.assertNotIn(
                'std::env::var("FLEET_GIT_SHA")',
                body,
                f"{rel} must not read FLEET_GIT_SHA at runtime",
            )
            self.assertNotIn(
                'env::var("FLEET_GIT_SHA")',
                body,
                f"{rel} must not read FLEET_GIT_SHA at runtime",
            )


if __name__ == "__main__":
    unittest.main(verbosity=2)
