#!/usr/bin/env python3
"""Install script contract: no args, repo-root .env, documented profiles."""
from __future__ import annotations

import hashlib
import os
import shlex
import subprocess
import tempfile
import unittest
from pathlib import Path

LAB = Path(__file__).resolve().parents[1]
REPO = LAB.parents[2]
SCRIPT = REPO / "scripts" / "install-full-suite-deps.sh"
WRAPPER = LAB / "scripts" / "install-full-suite-deps.sh"
FULL_SUITE = LAB / "scripts" / "run-full-suite.sh"


def _enable_kind_cluster_block(text: str) -> str:
    """ENABLE_KIND act that kind-loads :local images (not the kind-version probe)."""
    head = (
        'if [[ "${ENABLE_KIND}" == "1" ]]; then\n'
        '  echo "==> kind cluster ${KIND_CLUSTER}"'
    )
    start = text.find(head)
    if start < 0:
        raise AssertionError("ENABLE_KIND cluster block missing")
    tail = "\nfi\n\necho\nif ! as_user_sh"
    end = text.find(tail, start)
    if end < 0:
        raise AssertionError("ENABLE_KIND cluster block did not close before dx PATH check")
    return text[start:end]


def _enable_browser_e2e_block(text: str) -> str:
    """ENABLE_BROWSER_E2E act that installs Playwright OS libs and smokes Chromium."""
    head = 'if [[ "${ENABLE_BROWSER_E2E}" == "1" ]]; then\n'
    start = text.find(head)
    if start < 0:
        raise AssertionError("ENABLE_BROWSER_E2E block missing")
    end = text.find("\nfi\n", start)
    if end < 0:
        raise AssertionError("ENABLE_BROWSER_E2E block did not close")
    return text[start:end]


def _full_suite_browser_e2e_block(text: str) -> str:
    """run-full-suite.sh if/elif: e2e only when ENABLE=1 and SKIP_BROWSER!=1."""
    head = (
        'if [[ "${ENABLE_BROWSER_E2E}" == "1" && "${SKIP_BROWSER}" != "1" ]]; then\n'
    )
    start = text.find(head)
    if start < 0:
        raise AssertionError("ENABLE_BROWSER_E2E=1 && SKIP_BROWSER!=1 gate missing")
    end = text.find("\nfi\n", start)
    if end < 0:
        raise AssertionError("ENABLE_BROWSER_E2E gate did not close")
    return text[start:end]


def _bash_fn(text: str, name: str) -> str:
    """Product function body. Closing `}` must be at column 0."""
    start = text.find(f"{name}() {{")
    if start < 0:
        raise AssertionError(f"missing {name}()")
    end = text.find("\n}", start)
    if end < 0:
        raise AssertionError(f"unclosed {name}()")
    return text[start:end]


def _product_fn(text: str, name: str) -> str:
    """Product function including closer. One-liner or column-0 `}`."""
    start = text.find(f"{name}() {{")
    if start < 0:
        raise AssertionError(f"missing {name}()")
    nl = text.find("\n", start)
    if nl < 0:
        raise AssertionError(f"unclosed {name}()")
    if text[start:nl].rstrip().endswith("}"):
        return text[start:nl]
    end = text.find("\n}", start)
    if end < 0:
        raise AssertionError(f"unclosed {name}()")
    return text[start : end + 2]


def _resolved_enable(profile: str, name: str) -> str:
    """ENABLE_* after bash-eval of set_default + PROFILE case (empty env)."""
    text = SCRIPT.read_text(encoding="utf-8")
    start = text.find('PROFILE="${PROFILE:-')
    if start < 0:
        start = text.find('case "${PROFILE}" in')
    if start < 0:
        raise AssertionError("missing PROFILE case")
    case_at = text.find('case "${PROFILE}" in', start)
    if case_at < 0:
        raise AssertionError('missing case "${PROFILE}"')
    end = text.find("\nesac", case_at)
    if end < 0:
        raise AssertionError("unclosed PROFILE case")
    block = text[start : end + 5]
    ran = subprocess.run(
        ["bash", "-c", f"{block}\nprintf '%s\\n' \"${{{name}-}}\""],
        capture_output=True,
        text=True,
        env={"PROFILE": profile, "PATH": os.environ.get("PATH", "/usr/bin:/bin")},
        check=False,
    )
    if ran.returncode != 0:
        raise AssertionError(
            f"PROFILE={profile} {name}: {ran.stderr or ran.stdout or ran.returncode}"
        )
    return ran.stdout.strip()


def _run_docker_pull_or_have(*, image_local: bool):
    """Extract have_image + docker_pull_or_have; mock docker pull fail + sleep."""
    text = SCRIPT.read_text(encoding="utf-8")
    have_fn = _product_fn(text, "have_image")
    pull_fn = _product_fn(text, "docker_pull_or_have")
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        state = root / "state"
        state.mkdir()
        bin_dir = root / "bin"
        bin_dir.mkdir()
        (bin_dir / "docker").write_text(
            "#!/usr/bin/env bash\n"
            "set -euo pipefail\n"
            'STATE="${MOCK_DOCKER_STATE:?}"\n'
            'if [[ "${1:-}" == "pull" ]]; then\n'
            '  printf "pull %s\\n" "${2:-}" >> "${STATE}/log"\n'
            "  exit 1\n"
            "fi\n"
            'if [[ "${1:-}" == "image" && "${2:-}" == "inspect" ]]; then\n'
            '  printf "inspect %s\\n" "${3:-}" >> "${STATE}/log"\n'
            '  if [[ -f "${STATE}/local" ]]; then\n'
            "    exit 0\n"
            "  fi\n"
            "  exit 1\n"
            "fi\n"
            'echo "unexpected docker: $*" >&2\n'
            "exit 1\n",
            encoding="utf-8",
        )
        (bin_dir / "docker").chmod(0o755)
        (bin_dir / "sleep").write_text("#!/usr/bin/env bash\nexit 0\n", encoding="utf-8")
        (bin_dir / "sleep").chmod(0o755)
        if image_local:
            (state / "local").write_text("1\n", encoding="utf-8")
        wrapper = root / "run.sh"
        wrapper.write_text(
            "#!/usr/bin/env bash\n"
            "set -euo pipefail\n"
            f"{have_fn}\n"
            f"{pull_fn}\n"
            "docker_pull_or_have redis:7-alpine\n",
            encoding="utf-8",
        )
        env = os.environ.copy()
        env["PATH"] = f"{bin_dir}{os.pathsep}{env.get('PATH', '')}"
        env["MOCK_DOCKER_STATE"] = str(state)
        ran = subprocess.run(
            ["bash", str(wrapper)],
            capture_output=True,
            text=True,
            env=env,
            check=False,
        )
        log_path = state / "log"
        log = log_path.read_text(encoding="utf-8") if log_path.is_file() else ""
        return ran, log


_HARNESS_BASE_DF = "FROM alpine:3.21\n# harness base\n"
_HARNESS_NAMED_DF = "FROM agent-fleet-base:alpine\n# named harness\n"
_HARNESS_MOCK_DOCKER = r"""#!/usr/bin/env bash
set -euo pipefail
STATE="${MOCK_DOCKER_STATE:?}"
safe_tag() {
  local t="$1"
  t="${t//\//__}"
  t="${t//:/__}"
  printf '%s\n' "$t"
}
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
  safe="$(safe_tag "${tag}")"
  if [[ ! -e "${STATE}/images/${safe}" ]]; then
    exit 1
  fi
  if [[ -n "${fmt}" ]]; then
    key=""
    if [[ "${fmt}" == *fleet.harness_src* ]]; then
      key="harness_src"
    elif [[ "${fmt}" == *fleet.git_sha* ]]; then
      key="git_sha"
    fi
    if [[ -n "${key}" && -f "${STATE}/images/${safe}.${key}" ]]; then
      cat "${STATE}/images/${safe}.${key}"
    fi
  fi
  exit 0
fi
if [[ "${1:-}" == "build" ]]; then
  printf '%s\n' "$*" >> "${STATE}/builds"
  exit 0
fi
if [[ "${1:-}" == "run" ]]; then
  printf '%s\n' "$*" >> "${STATE}/runs"
  if [[ " $* " != *" --user 1000:1000 "* || " $* " != *" --version"* ]]; then
    echo "unexpected docker run: $*" >&2
    exit 1
  fi
  if [[ -f "${STATE}/builds" ]]; then
    echo "grok 0.0.0-test"
    exit 0
  fi
  echo "exec format error" >&2
  exit 1
fi
echo "unexpected docker: $*" >&2
exit 1
"""


def _grok_uid1000_probe_block(text: str) -> str:
    """ENABLE_HARNESS_GROK uid-1000 --version probe (FORCE_REBUILD on fail)."""
    head = (
        'if have_image agent-fleet-grok:alpine && '
        '[[ "${ENABLE_HARNESS_GROK}" == "1" ]]; then'
    )
    start = text.find(head)
    if start < 0:
        raise AssertionError("grok uid 1000 probe block missing")
    ok = 'echo "==> grok image ok as uid 1000:'
    ok_at = text.find(ok, start)
    if ok_at < 0:
        raise AssertionError("grok uid 1000 ok echo missing")
    end = text.find("\nfi\n", ok_at)
    if end < 0:
        raise AssertionError("grok uid 1000 probe block did not close")
    return text[start : end + 4]


def _run_harness_src_hash(
    *,
    file: str = "opencode",
    base_text: str = _HARNESS_BASE_DF,
    named_text: str = _HARNESS_NAMED_DF,
):
    """Extract harness_src_hash and run it against temp base + named Dockerfiles."""
    text = SCRIPT.read_text(encoding="utf-8")
    hash_fn = _product_fn(text, "harness_src_hash")
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        agents = root / "agents"
        (agents / "base").mkdir(parents=True)
        (agents / file).mkdir(parents=True, exist_ok=True)
        (agents / "base" / "Dockerfile").write_text(base_text, encoding="utf-8")
        if file != "base":
            (agents / file / "Dockerfile").write_text(named_text, encoding="utf-8")
        wrapper = root / "run.sh"
        wrapper.write_text(
            "#!/usr/bin/env bash\n"
            "set -euo pipefail\n"
            f"AGENTS={shlex.quote(str(agents))}\n"
            f"{hash_fn}\n"
            f"harness_src_hash {shlex.quote(file)}\n",
            encoding="utf-8",
        )
        ran = subprocess.run(
            ["bash", str(wrapper)],
            capture_output=True,
            text=True,
            check=False,
        )
        return ran


def _run_build_one_harness(
    *,
    name: str = "opencode",
    file: str | None = None,
    exists: bool = True,
    base_exists: bool = True,
    label: str | None = "match",
    git_sha: str | None = None,
    force: bool = False,
    base_text: str = _HARNESS_BASE_DF,
    named_text: str = _HARNESS_NAMED_DF,
    extra_body: str = "",
    extra_env: dict | None = None,
):
    """Extract have_image + harness_src_hash + build_one_harness; mock docker."""
    file = name if file is None else file
    text = SCRIPT.read_text(encoding="utf-8")
    have_fn = _product_fn(text, "have_image")
    hash_fn = _product_fn(text, "harness_src_hash")
    build_fn = _product_fn(text, "build_one_harness")
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        agents = root / "agents"
        (agents / "base").mkdir(parents=True)
        (agents / file).mkdir(parents=True, exist_ok=True)
        (agents / "base" / "Dockerfile").write_text(base_text, encoding="utf-8")
        if file != "base":
            (agents / file / "Dockerfile").write_text(named_text, encoding="utf-8")
        src_ran = subprocess.run(
            [
                "bash",
                "-c",
                f"{hash_fn}\nharness_src_hash {shlex.quote(file)}\n",
            ],
            capture_output=True,
            text=True,
            env={
                "AGENTS": str(agents),
                "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            },
            check=False,
        )
        if src_ran.returncode != 0:
            raise AssertionError(
                f"harness_src_hash: {src_ran.stderr or src_ran.stdout or src_ran.returncode}"
            )
        src = src_ran.stdout.strip()
        state = root / "state"
        (state / "images").mkdir(parents=True)
        tmp_root = root / "tmp"
        tmp_root.mkdir()

        def plant(tag: str, *, harness_src: str | None, git_sha_val: str | None) -> None:
            safe = tag.replace("/", "__").replace(":", "__")
            (state / "images" / safe).write_text("1\n", encoding="utf-8")
            if harness_src is not None:
                (state / "images" / f"{safe}.harness_src").write_text(
                    harness_src, encoding="utf-8"
                )
            if git_sha_val is not None:
                (state / "images" / f"{safe}.git_sha").write_text(
                    git_sha_val, encoding="utf-8"
                )

        if base_exists:
            plant("agent-fleet-base:alpine", harness_src=src, git_sha_val=None)
        if exists:
            planted = src if label == "match" else label
            plant(
                f"agent-fleet-{name}:alpine",
                harness_src=planted,
                git_sha_val=git_sha,
            )
        bin_dir = root / "bin"
        bin_dir.mkdir()
        docker = bin_dir / "docker"
        docker.write_text(_HARNESS_MOCK_DOCKER, encoding="utf-8")
        docker.chmod(0o755)
        body = extra_body if extra_body else (
            f"build_one_harness {shlex.quote(name)} {shlex.quote(file)}\n"
        )
        wrapper = root / "run.sh"
        wrapper.write_text(
            "#!/usr/bin/env bash\n"
            "set -euo pipefail\n"
            f"AGENTS={shlex.quote(str(agents))}\n"
            f"TMP_ROOT={shlex.quote(str(tmp_root))}\n"
            f"FORCE_REBUILD={1 if force else 0}\n"
            'ENABLE_HARNESS_GROK="${ENABLE_HARNESS_GROK:-0}"\n'
            "mkdir -p \"${TMP_ROOT}\"\n"
            'as_user() { "$@"; }\n'
            f"{have_fn}\n"
            f"{hash_fn}\n"
            f"{build_fn}\n"
            f"{body}",
            encoding="utf-8",
        )
        env = os.environ.copy()
        env["PATH"] = f"{bin_dir}{os.pathsep}{env.get('PATH', '')}"
        env["MOCK_DOCKER_STATE"] = str(state)
        if extra_env:
            env.update(extra_env)
        ran = subprocess.run(
            ["bash", str(wrapper)],
            capture_output=True,
            text=True,
            env=env,
            check=False,
        )
        built = ""
        builds = state / "builds"
        if builds.is_file():
            built = builds.read_text(encoding="utf-8")
        runs = ""
        runs_path = state / "runs"
        if runs_path.is_file():
            runs = runs_path.read_text(encoding="utf-8")
        return ran, built, src, runs


def _enable_ollama_block(text: str) -> str:
    """ENABLE_OLLAMA act: reuse healthy public :11434 or start, then /api/tags."""
    head = 'if [[ "${ENABLE_OLLAMA}" == "1" ]]; then\n'
    start = text.find(head)
    if start < 0:
        raise AssertionError("ENABLE_OLLAMA block missing")
    tail = "\n# --- :1234 OpenAI-compat proxy"
    end = text.find(tail, start)
    if end < 0:
        raise AssertionError("ENABLE_OLLAMA block did not close before :1234 proxy")
    return text[start:end]


def _run_enable_ollama(*, listen: str, tags_ok: bool, v1_ok: bool = True):
    """Extract ENABLE_OLLAMA helpers + block; mock ss/curl/systemctl/pkill."""
    text = SCRIPT.read_text(encoding="utf-8")
    need_fn = _product_fn(text, "need_cmd")
    have_fn = _product_fn(text, "have_systemd")
    listen_fn = _product_fn(text, "ollama_listen")
    public_fn = _product_fn(text, "ollama_listen_is_public")
    tags_fn = _product_fn(text, "ollama_tags_ok")
    start_fn = _product_fn(text, "ollama_systemd_start")
    block = _enable_ollama_block(text)
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        state = root / "state"
        state.mkdir()
        tmp_root = root / "tmp"
        tmp_root.mkdir()
        bin_dir = root / "bin"
        bin_dir.mkdir()
        if tags_ok:
            (state / "tags_ok").write_text("1\n", encoding="utf-8")
        if v1_ok:
            (state / "v1_ok").write_text("1\n", encoding="utf-8")

        def plant(name: str, body: str) -> None:
            p = bin_dir / name
            p.write_text(body, encoding="utf-8")
            p.chmod(0o755)

        plant(
            "ss",
            "#!/usr/bin/env bash\n"
            "set -euo pipefail\n"
            'addr="${MOCK_SS_LISTEN:-}"\n'
            'if [[ -n "${addr}" ]]; then\n'
            "  printf 'LISTEN 0 0 %s 0.0.0.0:*\\n' \"${addr}\"\n"
            "fi\n"
            "exit 0\n",
        )
        plant(
            "curl",
            "#!/usr/bin/env bash\n"
            "set -euo pipefail\n"
            'STATE="${MOCK_OLLAMA_STATE:?}"\n'
            'printf "curl %s\\n" "$*" >> "${STATE}/log"\n'
            'url=""\n'
            'for a in "$@"; do\n'
            '  case "$a" in\n'
            '    http*) url="$a" ;;\n'
            "  esac\n"
            "done\n"
            'if [[ "${url}" == *"/api/tags" ]]; then\n'
            '  if [[ -f "${STATE}/tags_ok" ]]; then\n'
            "    exit 0\n"
            "  fi\n"
            "  exit 1\n"
            "fi\n"
            'if [[ "${url}" == *"/v1/models" ]]; then\n'
            '  if [[ -f "${STATE}/v1_ok" ]]; then\n'
            "    exit 0\n"
            "  fi\n"
            "  exit 1\n"
            "fi\n"
            'echo "unexpected curl: $*" >&2\n'
            "exit 1\n",
        )
        plant(
            "systemctl",
            "#!/usr/bin/env bash\n"
            "set -euo pipefail\n"
            'STATE="${MOCK_OLLAMA_STATE:?}"\n'
            'printf "systemctl %s\\n" "$*" >> "${STATE}/log"\n'
            'if [[ "${1:-}" == "is-system-running" ]]; then\n'
            "  echo running\n"
            "  exit 0\n"
            "fi\n"
            'if [[ "${1:-}" == "is-failed" ]]; then\n'
            "  exit 1\n"
            "fi\n"
            "exit 0\n",
        )
        plant(
            "pkill",
            "#!/usr/bin/env bash\n"
            "set -euo pipefail\n"
            'STATE="${MOCK_OLLAMA_STATE:?}"\n'
            'printf "pkill %s\\n" "$*" >> "${STATE}/log"\n'
            "exit 0\n",
        )
        plant("sleep", "#!/usr/bin/env bash\nexit 0\n")
        plant(
            "ollama",
            "#!/usr/bin/env bash\n"
            "set -euo pipefail\n"
            'STATE="${MOCK_OLLAMA_STATE:?}"\n'
            'printf "ollama %s\\n" "$*" >> "${STATE}/log"\n'
            "exit 0\n",
        )
        plant(
            "id",
            "#!/usr/bin/env bash\n"
            "set -euo pipefail\n"
            "printf '0\\n'\n"
            "exit 0\n",
        )
        wrapper = root / "run.sh"
        wrapper.write_text(
            "#!/usr/bin/env bash\n"
            "set -euo pipefail\n"
            "OS_FAMILY=linux\n"
            "ENABLE_OLLAMA=1\n"
            "REAL_USER=tester\n"
            "OLLAMA_MODEL=tiny\n"
            f"TMP_ROOT={shlex.quote(str(tmp_root))}\n"
            f"{need_fn}\n"
            f"{have_fn}\n"
            'as_user() { "$@"; }\n'
            'as_user_sh() { bash -c "$*"; }\n'
            f"{listen_fn}\n"
            f"{public_fn}\n"
            f"{tags_fn}\n"
            'ollama_write_host_dropin() { printf "dropin\\n" >> "${MOCK_OLLAMA_STATE}/log"; }\n'
            f"{start_fn}\n"
            'ollama_start_detached() { printf "detached\\n" >> "${MOCK_OLLAMA_STATE}/log"; }\n'
            f"{block}\n",
            encoding="utf-8",
        )
        env = os.environ.copy()
        env["PATH"] = f"{bin_dir}{os.pathsep}{env.get('PATH', '')}"
        env["MOCK_OLLAMA_STATE"] = str(state)
        env["MOCK_SS_LISTEN"] = listen
        ran = subprocess.run(
            ["bash", str(wrapper)],
            capture_output=True,
            text=True,
            env=env,
            check=False,
        )
        log_path = state / "log"
        log = log_path.read_text(encoding="utf-8") if log_path.is_file() else ""
        return ran, log


class InstallContractTests(unittest.TestCase):
    def test_script_exists_and_executable(self):
        self.assertTrue(SCRIPT.is_file(), SCRIPT)
        self.assertTrue(SCRIPT.stat().st_mode & 0o111, "script must be executable")
        self.assertTrue(WRAPPER.is_file(), WRAPPER)
        self.assertTrue(WRAPPER.stat().st_mode & 0o111, "leftover wrapper must be executable")
        wrap = WRAPPER.read_text(encoding="utf-8")
        self.assertIn("scripts/install-full-suite-deps.sh", wrap)
        self.assertRegex(wrap, r"(?m)^exec ")

    def test_kind_rollout_after_load(self):
        kind = _enable_kind_cluster_block(SCRIPT.read_text(encoding="utf-8"))
        load_at = kind.find("kind load docker-image")
        self.assertGreaterEqual(load_at, 0, "ENABLE_KIND must kind load :local images")
        restart_at = kind.find("rollout restart", load_at)
        self.assertGreater(restart_at, load_at, "must rollout restart after kind load")
        restart_line = kind[restart_at:].split("\n", 1)[0]
        for deploy in ("deploy/api", "deploy/ui", "deploy/fleet-operator"):
            self.assertIn(deploy, restart_line, f"restart must include {deploy}")
            status_at = kind.find(f"rollout status {deploy}", restart_at)
            self.assertGreater(status_at, restart_at, f"{deploy} status after restart")
        # Ready wait on pre-existing pods is not a this-tree check.
        self.assertEqual(
            kind.find("wait --for=condition=Ready pods --all"),
            -1,
            "do not wait Ready on possibly-old pods",
        )

    def test_run_every_test_prepare_kind_rollout_after_load(self):
        every = (REPO / "scripts" / "run-every-test.sh").read_text(encoding="utf-8")
        prepare = _bash_fn(every, "prepare_kind")
        load_at = prepare.find("kind load docker-image")
        restart_at = prepare.find("rollout restart")
        self.assertGreaterEqual(load_at, 0, "prepare_kind must kind load :local images")
        self.assertGreater(restart_at, load_at, "prepare_kind must restart after kind load")
        restart_span = prepare[restart_at:]
        status_at = restart_span.find("rollout status")
        if status_at >= 0:
            restart_span = restart_span[:status_at]
        self.assertIn("deploy/api", restart_span)
        self.assertEqual(
            prepare.find("wait --for=condition=Ready pods --all"),
            -1,
            "do not wait Ready on possibly-old pods",
        )

    def test_install_kind_stack_identity_after_rollout(self):
        kind = _enable_kind_cluster_block(SCRIPT.read_text(encoding="utf-8"))
        restart_at = kind.find("rollout restart")
        ident_at = kind.find('python3 "${REPO}/tests/browser/stack_identity.py"')
        self.assertGreaterEqual(restart_at, 0, "ENABLE_KIND must rollout restart")
        self.assertGreater(
            ident_at,
            restart_at,
            "stack_identity.py must run after rollout restart",
        )
        fail_at = kind.find("kind host :8080/:3030 is not this tree")
        self.assertGreater(fail_at, ident_at, "identity failure must follow stack_identity.py")
        self.assertIn("exit 1", kind[fail_at:])

    def test_docker_pull_retries_and_uses_local_cache(self):
        ran, log = _run_docker_pull_or_have(image_local=True)
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        pulls = [ln for ln in log.splitlines() if ln.startswith("pull ")]
        self.assertEqual(pulls, ["pull redis:7-alpine"] * 3, log)
        self.assertIn("inspect redis:7-alpine", log)

    def test_docker_pull_or_have_exits_1_when_three_pulls_fail_and_image_absent(self):
        ran, log = _run_docker_pull_or_have(image_local=False)
        self.assertEqual(ran.returncode, 1, ran.stdout + ran.stderr)
        self.assertIn("no local image", ran.stderr)
        pulls = [ln for ln in log.splitlines() if ln.startswith("pull ")]
        self.assertEqual(pulls, ["pull redis:7-alpine"] * 3, log)

    def test_install_routes_all_base_images_through_docker_pull_or_have(self):
        text = SCRIPT.read_text(encoding="utf-8")
        for ref in (
            "redis:7-alpine",
            "python:3.13-bookworm",
            "alpine:3.21",
            '"${RUST_IMAGE}"',
            "postgres:16-alpine",
        ):
            self.assertIn(
                f"docker_pull_or_have {ref}",
                text,
                f"{ref} must go through docker_pull_or_have",
            )
            self.assertNotIn(
                f"docker pull {ref}",
                text,
                f"bare docker pull {ref} aborts install on registry 502",
            )

    def test_kubectl_kind_stage_under_repo_tmp(self):
        text = SCRIPT.read_text(encoding="utf-8")
        self.assertIn('TMP_ROOT="${REPO}/.tmp"', text)
        self.assertIn("install_upstream_bin kubectl", text)
        self.assertIn("install_upstream_bin kind", text)

    def test_browser_e2e_default_full_only(self):
        self.assertEqual(
            _resolved_enable("full", "ENABLE_BROWSER_E2E"),
            "1",
            "PROFILE=full should default ENABLE_BROWSER_E2E=1",
        )
        self.assertEqual(
            _resolved_enable("lab", "ENABLE_BROWSER_E2E"),
            "0",
            "PROFILE=lab is compose-only; do not default browser e2e on",
        )
        self.assertEqual(
            _resolved_enable("minimal", "ENABLE_BROWSER_E2E"),
            "0",
            "PROFILE=minimal should default ENABLE_BROWSER_E2E=0",
        )

    def test_playwright_installs_os_deps(self):
        text = SCRIPT.read_text(encoding="utf-8")
        pw = _product_fn(text, "install_playwright_os_libs")
        # Matrix: every documented Linux PM + darwin/windows no-op.
        for token in (
            "need_cmd apt-get",
            "need_cmd dnf",
            "need_cmd pacman",
            "need_cmd zypper",
            "need_cmd apk",
            "darwin)",
            "windows)",
        ):
            self.assertIn(token, pw, token)
        # Required Chromium .so families, not a --skip-unavailable dump.
        self.assertNotIn("--skip-unavailable", pw)
        for lib in ("nss", "gtk", "gbm", "asound"):
            self.assertIn(lib, pw, lib)
        e2e = _enable_browser_e2e_block(text)
        self.assertIn("chromium.launch(headless=True)", e2e)
        stub = (REPO / "scripts" / "sudo-playwright-deps.sh").read_text(encoding="utf-8")
        self.assertIn("install-full-suite-deps.sh", stub)
        self.assertNotIn("python3 -m playwright install-deps", stub)

    def test_enable_browser_e2e_block_calls_install_playwright_os_libs(self):
        e2e = _enable_browser_e2e_block(SCRIPT.read_text(encoding="utf-8"))
        self.assertRegex(
            e2e,
            r"(?m)^\s*install_playwright_os_libs\s*$",
            "ENABLE_BROWSER_E2E=1 must invoke the OS-lib installer",
        )

    def test_playwright_install_deps_only_inside_apt_branch(self):
        pw = _product_fn(SCRIPT.read_text(encoding="utf-8"), "install_playwright_os_libs")
        apt_head = 'if [[ "${pm}" == apt ]]; then'
        apt_at = pw.find(apt_head)
        self.assertGreaterEqual(apt_at, 0, "apt install-deps branch missing")
        deps = "python3 -m playwright install-deps chromium"
        deps_at = pw.find(deps, apt_at)
        self.assertGreater(deps_at, apt_at, "install-deps must run only under pm==apt")
        close_at = pw.find("\n  fi\n", deps_at)
        self.assertGreater(close_at, deps_at, "apt install-deps branch did not close")
        outside = pw[:apt_at] + pw[close_at:]
        self.assertNotIn(deps, outside)

    def test_sudo_playwright_deps_sh_exits_2(self):
        stub = REPO / "scripts" / "sudo-playwright-deps.sh"
        ran = subprocess.run(
            ["bash", str(stub)],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(ran.returncode, 2, ran.stdout + ran.stderr)

    def test_full_suite_gates_browser_e2e(self):
        text = FULL_SUITE.read_text(encoding="utf-8")
        self.assertIn('ROOT_ENV="${REPO}/.env"', text)
        self.assertIn('. "${ROOT_ENV}"', text)
        gate = _full_suite_browser_e2e_block(text)
        elif_at = gate.find("\nelif ")
        self.assertGreater(elif_at, 0, "SKIP_BROWSER=1 elif missing")
        run = gate[:elif_at]
        skip = gate[elif_at:]
        self.assertIn('python3 "${REPO}/tests/browser/stack_identity.py"', run)
        self.assertIn('python3 "${REPO}/tests/browser/test_app_e2e.py"', run)
        self.assertNotIn("tests/browser/test_app_e2e.py", skip)
        self.assertNotIn(
            "tests/browser/test_app_e2e.py",
            text.replace(gate, "", 1),
        )
        self.assertIn('elif [[ "${ENABLE_BROWSER_E2E}" == "1" ]]', skip)
        self.assertIn("ENABLE_BROWSER_E2E!=1", skip)

    def test_full_suite_browser_e2e_identity_then_app_e2e_fail_closed(self):
        gate = _full_suite_browser_e2e_block(FULL_SUITE.read_text(encoding="utf-8"))
        tools = 'python3 "${REPO}/tests/browser/stack_identity.py" --tools'
        ident = 'python3 "${REPO}/tests/browser/stack_identity.py"; then'
        e2e = 'python3 "${REPO}/tests/browser/test_app_e2e.py"'
        tools_at = gate.find(tools)
        ident_at = gate.find(ident)
        e2e_at = gate.find(e2e)
        self.assertGreaterEqual(tools_at, 0, "must run stack_identity.py --tools")
        self.assertGreater(ident_at, tools_at, "stack_identity.py must follow --tools")
        self.assertGreater(e2e_at, ident_at, "test_app_e2e.py must follow identity")
        self.assertGreater(
            gate.find("fail ", tools_at),
            tools_at,
            "non-zero --tools must call fail",
        )
        self.assertGreater(
            gate.find("fail ", ident_at),
            ident_at,
            "non-zero identity must call fail",
        )
        self.assertGreater(
            gate.find("fail ", e2e_at),
            e2e_at,
            "non-zero test_app_e2e.py must call fail",
        )
        self.assertNotIn("|| true", gate)

    def test_full_suite_defaults_enable_browser_e2e_off(self):
        text = FULL_SUITE.read_text(encoding="utf-8")
        self.assertIn('ENABLE_BROWSER_E2E="${ENABLE_BROWSER_E2E:-0}"', text)

    def test_required_packages_are_not_skip_unavailable(self):
        pkgs = _product_fn(SCRIPT.read_text(encoding="utf-8"), "install_packages")
        dnf_at = pkgs.find("if need_cmd dnf; then")
        apt_at = pkgs.find("elif need_cmd apt-get; then")
        pacman_at = pkgs.find("elif need_cmd pacman; then")
        self.assertGreaterEqual(dnf_at, 0, "dnf install_packages branch missing")
        self.assertGreater(apt_at, dnf_at, "apt-get branch must follow dnf")
        self.assertGreater(pacman_at, apt_at, "pacman branch must follow apt-get")
        dnf = pkgs[dnf_at:apt_at]
        apt = pkgs[apt_at:pacman_at]
        # Unique NEVRAs: any flag order of --skip-unavailable drops required rpms.
        self.assertNotRegex(
            dnf,
            r"dnf install[^\n]*--skip-unavailable",
            "unique NEVRA dnf install must not skip-unavailable",
        )
        self.assertIn("dnf install -y ${pkgs}", dnf)
        self.assertRegex(dnf, r"for p in[\s\S]*\bpython3-redis\b[\s\S]*; do")
        self.assertRegex(dnf, r"for p in[\s\S]*\blibpq-devel\b[\s\S]*; do")
        v2 = "apt-get install -y docker-compose-v2"
        v2_at = apt.find(v2)
        self.assertGreaterEqual(v2_at, 0, "apt must install docker-compose-v2")
        self.assertIn(
            "apt-get install -y docker-compose",
            apt[v2_at + len(v2) :],
            "docker-compose is the apt fallback after v2",
        )
        apt_cmd = apt.replace("\\\n", " ")
        self.assertRegex(
            apt_cmd,
            r"apt-get install -y\s+docker\.io\b[^\n]*\bpython3-redis\b",
        )
        self.assertRegex(
            apt_cmd,
            r"apt-get install -y\s+docker\.io\b[^\n]*\blibpq-dev\b",
        )

    def test_pm_install_required_errors_when_no_alias_matches(self):
        fn = _product_fn(SCRIPT.read_text(encoding="utf-8"), "_pm_install")
        opt = 'if [[ "${optional}" -eq 1 ]]; then'
        opt_at = fn.find(opt)
        self.assertGreaterEqual(opt_at, 0, "_pm_install must distinguish --optional")
        fi_at = fn.find("\n  fi\n", opt_at)
        self.assertGreater(fi_at, opt_at, "optional branch did not close")
        tail = fn[fi_at:]
        self.assertIn("ERROR: could not install any of", tail)
        self.assertIn("return 1", tail)
        self.assertNotIn("return 0", tail)

    def test_install_apt_required_list_includes_redis_server(self):
        pkgs = _product_fn(SCRIPT.read_text(encoding="utf-8"), "install_packages")
        apt_at = pkgs.find("elif need_cmd apt-get; then")
        pacman_at = pkgs.find("elif need_cmd pacman; then")
        self.assertGreaterEqual(apt_at, 0, "apt-get install_packages branch missing")
        self.assertGreater(pacman_at, apt_at, "pacman branch must follow apt-get")
        apt = pkgs[apt_at:pacman_at].replace("\\\n", " ")
        self.assertRegex(
            apt,
            r"apt-get install -y\s+docker\.io\b[^\n]*\bredis-server\b",
            "apt required install -y list must include redis-server",
        )

    def test_harness_images_skip_only_on_src_hash(self):
        """Skip alpine harness only when label fleet.harness_src == Dockerfile hash."""
        build_fn = _product_fn(SCRIPT.read_text(encoding="utf-8"), "build_one_harness")
        self.assertIn('index .Config.Labels "fleet.harness_src"', build_fn)
        self.assertRegex(
            build_fn,
            r'\[\[ -n "\$\{src\}" && "\$\{have\}" == "\$\{src\}" \]\]',
            "skip compare must be inspect fleet.harness_src == harness_src_hash",
        )
        self.assertNotIn("fleet.git_sha", build_fn)
        self.assertNotIn("tree-id.sh", build_fn)

        ran, built, src, _runs = _run_build_one_harness(exists=True, label="match")
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        self.assertRegex(src, r"^[0-9a-f]{64}$")
        self.assertEqual(built, "", "matching fleet.harness_src may skip rebuild")
        self.assertIn("skip", ran.stdout)
        self.assertIn("fleet.harness_src", ran.stdout)

        ran, built, src, _runs = _run_build_one_harness(exists=True, label="deadbeef" * 8)
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        self.assertIn("build", built, "stale fleet.harness_src must rebuild")
        self.assertIn("-t agent-fleet-opencode:alpine", built)
        self.assertIn(f"fleet.harness_src={src}", built)
        self.assertIn("rebuild", ran.stdout)

        ran, built, src, _runs = _run_build_one_harness(exists=True, label=None)
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        self.assertIn("build", built, "unset fleet.harness_src must rebuild")
        self.assertIn("-t agent-fleet-opencode:alpine", built)

        ran, built, src, _runs = _run_build_one_harness(
            exists=True, label="match", force=True
        )
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        self.assertIn("build", built, "FORCE_REBUILD=1 must rebuild a matching label")
        self.assertIn("-t agent-fleet-opencode:alpine", built)

        ran, built, _src, _runs = _run_build_one_harness(exists=False, label=None)
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        self.assertIn("build", built, "missing alpine tag must rebuild")
        self.assertIn("-t agent-fleet-opencode:alpine", built)

    def test_harness_src_hash_is_sha256_of_base_then_named_dockerfile(self):
        """A base Dockerfile edit must invalidate every agent-fleet-*:alpine tag."""
        hash_fn = _product_fn(SCRIPT.read_text(encoding="utf-8"), "harness_src_hash")
        self.assertIn(
            'cat "${AGENTS}/base/Dockerfile" "${AGENTS}/${file}/Dockerfile"',
            hash_fn,
        )
        base = _HARNESS_BASE_DF
        named = _HARNESS_NAMED_DF
        ran = _run_harness_src_hash(file="opencode", base_text=base, named_text=named)
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        got = ran.stdout.strip()
        want = hashlib.sha256((base + named).encode("utf-8")).hexdigest()
        named_only = hashlib.sha256(named.encode("utf-8")).hexdigest()
        base_only = hashlib.sha256(base.encode("utf-8")).hexdigest()
        self.assertEqual(got, want)
        self.assertNotEqual(got, named_only, "src must not be only the named Dockerfile")
        self.assertNotEqual(got, base_only, "src must include the named Dockerfile")

        base2 = "FROM alpine:3.21\n# harness base edited\n"
        ran2 = _run_harness_src_hash(file="opencode", base_text=base2, named_text=named)
        self.assertEqual(ran2.returncode, 0, ran2.stdout + ran2.stderr)
        want2 = hashlib.sha256((base2 + named).encode("utf-8")).hexdigest()
        self.assertEqual(ran2.stdout.strip(), want2)
        self.assertNotEqual(got, want2, "base Dockerfile edit must change src")

    def test_build_one_harness_does_not_skip_on_fleet_git_sha_or_tag_existence(self):
        """Existence-only and fleet.git_sha are the two wrong skip keys."""
        tree = "0123456789abcdef0123456789abcdef01234567"
        ran, built, src, _runs = _run_build_one_harness(
            exists=True, label=None, git_sha=tree
        )
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        self.assertIn(
            "build",
            built,
            "inspect-success + matching tree-id without fleet.harness_src must rebuild",
        )
        self.assertIn("-t agent-fleet-opencode:alpine", built)
        self.assertNotEqual(src, tree)

        ran, built, src, _runs = _run_build_one_harness(
            exists=True, label="stale-harness-src", git_sha=tree
        )
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        self.assertIn(
            "build",
            built,
            "matching fleet.git_sha + stale fleet.harness_src must rebuild",
        )
        self.assertIn("-t agent-fleet-opencode:alpine", built)
        self.assertIn(f"fleet.harness_src={src}", built)

    def test_grok_uid1000_version_probe_forces_rebuild(self):
        """Hash-matching skip is not enough if uid 1000 grok --version fails."""
        text = SCRIPT.read_text(encoding="utf-8")
        probe = _grok_uid1000_probe_block(text)
        self.assertIn("--user 1000:1000", probe)
        self.assertIn("FORCE_REBUILD=1 build_one_harness grok grok", probe)
        body = "build_one_harness grok grok\n" + probe + "\n"
        ran, built, src, runs = _run_build_one_harness(
            name="grok",
            exists=True,
            label="match",
            extra_body=body,
            extra_env={"ENABLE_HARNESS_GROK": "1"},
        )
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        self.assertIn("skip", ran.stdout)
        self.assertIn("grok image not executable as uid 1000", ran.stdout)
        self.assertIn("build", built, "failing uid 1000 grok --version must rebuild")
        self.assertIn("-t agent-fleet-grok:alpine", built)
        self.assertIn(f"fleet.harness_src={src}", built)
        self.assertIn("--user 1000:1000", runs)
        self.assertIn("--version", runs)
        self.assertGreaterEqual(runs.count("\n"), 1)

    def test_does_not_copy_host_grok_auth_into_lab(self):
        copy_cmds = {"cp", "install", "ln", "rsync"}
        rm_cmds = {"rm", "unlink"}
        leftover = ("${LAB}/.grok-auth.json", "$LAB/.grok-auth.json")
        removed = False
        leaks = []
        for raw in SCRIPT.read_text(encoding="utf-8").splitlines():
            line = raw.split("#", 1)[0].strip()
            if not line:
                continue
            try:
                parts = shlex.split(line)
            except ValueError:
                parts = line.split()
            idx = next((i for i, p in enumerate(parts) if p in copy_cmds | rm_cmds), None)
            if idx is None:
                continue
            verb = parts[idx]
            args = [p for p in parts[idx + 1 :] if p != "--" and not p.startswith("-")]
            bare = [a.replace('"', "").replace("'", "") for a in args]
            if verb in rm_cmds and any(a.endswith(s) for a in bare for s in leftover):
                removed = True
            if verb not in copy_cmds or not bare:
                continue
            dest = bare[-1]
            if "${LAB}" not in dest and not dest.startswith("$LAB"):
                continue
            for src in bare[:-1]:
                if (
                    src.endswith(".grok/auth.json")
                    or src.endswith(".grok-auth.json")
                    or src.rstrip("/").endswith(".grok")
                    or src in ("$GROK_AUTH_PATH", "${GROK_AUTH_PATH}")
                ):
                    leaks.append(line)
                    break
        self.assertTrue(
            removed, "install must remove leftover ${LAB}/.grok-auth.json"
        )
        self.assertEqual(
            leaks, [], "must not copy/link/install host grok auth into ${LAB}"
        )

    def test_sandbox_compose_does_not_bind_mount_host_grok_auth(self):
        leaks = []
        in_volumes = False
        vol_indent = None
        compose = (LAB / "docker-compose.yml").read_text(encoding="utf-8")
        for raw in compose.splitlines():
            line = raw.split("#", 1)[0].rstrip()
            if not line.strip():
                continue
            indent = len(line) - len(line.lstrip(" "))
            if in_volumes and indent <= vol_indent:
                in_volumes = False
            if line.strip() == "volumes:":
                in_volumes = True
                vol_indent = indent
                continue
            if not in_volumes:
                continue
            entry = line.strip()
            if (
                ".grok/auth.json" in entry
                or ".grok-auth.json" in entry
                or "${GROK_AUTH_PATH}" in entry
                or "$GROK_AUTH_PATH" in entry
                or "/.grok:" in entry
                or entry.rstrip("\"'").endswith("/.grok")
                or "~/.grok" in entry
            ):
                leaks.append(entry)
        self.assertEqual(
            leaks,
            [],
            "docker-compose.yml volumes must not bind-mount host grok auth",
        )

    def test_kubectl_kind_version_not_swallowed(self):
        text = SCRIPT.read_text(encoding="utf-8")
        install_at = text.find("install_upstream_bin()")
        self.assertGreaterEqual(install_at, 0, "install_upstream_bin missing")
        after = text[install_at:]
        kubectl = "as_user kubectl version --client=true"
        kubectl_at = after.find(kubectl)
        self.assertGreaterEqual(
            kubectl_at,
            0,
            "as_user kubectl version --client=true must follow install_upstream_bin",
        )
        line_start = after.rfind("\n", 0, kubectl_at) + 1
        self.assertEqual(
            after[line_start:kubectl_at],
            "",
            "kubectl version probe must be unconditional",
        )
        kubectl_line = after[kubectl_at:].split("\n", 1)[0].rstrip()
        self.assertFalse(
            kubectl_line.endswith("|| true") or kubectl_line.endswith("|| :"),
            f"kubectl version must not be swallowed: {kubectl_line}",
        )
        kind_head = 'if [[ "${ENABLE_KIND}" == "1" ]]; then\n'
        kind_at = after.find(kind_head, kubectl_at)
        self.assertGreater(
            kind_at,
            kubectl_at,
            "ENABLE_KIND=1 kind version gate must follow kubectl probe",
        )
        kind_end = after.find("\nfi\n", kind_at)
        self.assertGreater(kind_end, kind_at, "ENABLE_KIND kind version block did not close")
        kind_block = after[kind_at:kind_end]
        kind = "as_user kind version"
        kind_line_at = kind_block.find(kind)
        self.assertGreaterEqual(
            kind_line_at,
            0,
            "as_user kind version must run inside ENABLE_KIND=1",
        )
        kind_line = kind_block[kind_line_at:].split("\n", 1)[0].rstrip()
        self.assertFalse(
            kind_line.endswith("|| true") or kind_line.endswith("|| :"),
            f"kind version must not be swallowed: {kind_line}",
        )

    def test_ollama_reuses_healthy_listener(self):
        ran, log = _run_enable_ollama(listen="0.0.0.0:11434", tags_ok=True)
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        pkills = [ln for ln in log.splitlines() if ln.startswith("pkill ")]
        self.assertEqual(pkills, [], log)
        bounced = [
            ln
            for ln in log.splitlines()
            if ln.startswith("systemctl start ") or ln.startswith("systemctl restart ")
        ]
        self.assertEqual(bounced, [], log)
        self.assertNotIn("detached", log.splitlines())

    def test_enable_ollama_exits_1_when_api_tags_never_up(self):
        ran, _log = _run_enable_ollama(listen="0.0.0.0:11434", tags_ok=False)
        self.assertEqual(ran.returncode, 1, ran.stdout + ran.stderr)

    def test_ollama_listen_is_public_accepts_only_wildcard_11434(self):
        fn = _product_fn(SCRIPT.read_text(encoding="utf-8"), "ollama_listen_is_public")
        ran = subprocess.run(
            [
                "bash",
                "-c",
                (
                    f"{fn}\n"
                    "ollama_listen_is_public 0.0.0.0:11434; echo v4=$?\n"
                    "ollama_listen_is_public '[::]:11434'; echo v6=$?\n"
                    "ollama_listen_is_public '*:11434'; echo star=$?\n"
                    "ollama_listen_is_public 127.0.0.1:11434; echo loop=$?\n"
                    "ollama_listen_is_public ''; echo empty=$?\n"
                ),
            ],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        self.assertEqual(
            ran.stdout,
            "v4=0\nv6=0\nstar=0\nloop=1\nempty=1\n",
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
