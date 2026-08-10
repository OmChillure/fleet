#!/usr/bin/env python3
"""Live kind cluster smoke. Skips when cluster is not installed."""
from __future__ import annotations

import os
import shutil
import subprocess
import sys
import urllib.error
import urllib.request
import unittest
from pathlib import Path

_REPO = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(_REPO / "tests" / "browser"))
from stack_identity import ForeignStackError, require_this_tree  # noqa: E402

_LAB_TESTS = Path(__file__).resolve().parent
sys.path.insert(0, str(_LAB_TESTS))
from lab_env import env_flag  # noqa: E402


def _kubectl(*args: str) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    # Do not inherit a leaked HOME from gitssh live tests.
    kube = Path.home() / ".kube" / "config"
    if not kube.is_file():
        kube = Path(os.path.expanduser("~")) / ".kube" / "config"
    if kube.is_file():
        env["KUBECONFIG"] = str(kube)
    ctx = os.environ.get("KIND_KUBE_CONTEXT", "kind-agent-fleet")
    cmd = ["kubectl"]
    if ctx:
        cmd.extend(["--context", ctx])
    cmd.extend(args)
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        timeout=30,
        env=env,
    )


def _cluster() -> bool:
    """True only for a live agent-fleet ns (kind or equivalent), not host cargo run."""
    if not shutil.which("kubectl"):
        return False
    ns = _kubectl("get", "ns", "agent-fleet", "-o", "name")
    if ns.returncode != 0 or "agent-fleet" not in (ns.stdout or ""):
        return False
    if shutil.which("kind"):
        try:
            k = subprocess.run(
                ["kind", "get", "clusters"],
                capture_output=True,
                text=True,
                timeout=15,
            )
        except (OSError, subprocess.TimeoutExpired):
            return False
        if k.returncode != 0:
            return False
        names = {ln.strip() for ln in k.stdout.splitlines() if ln.strip()}
        if not names:
            return False
    return True


class KindSmokeContractTests(unittest.TestCase):
    def test_detection_is_not_host_http_health(self):
        import inspect

        src = inspect.getsource(_cluster)
        self.assertIn("agent-fleet", src)
        self.assertIn("kubectl", src)
        self.assertNotIn("127.0.0.1:8080", src)
        self.assertNotIn("/health", src)


class KindSmokeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if _cluster():
            return
        if env_flag("ENABLE_KIND"):
            raise AssertionError(
                "ENABLE_KIND=1 but kind/agent-fleet namespace is not available"
            )
        raise unittest.SkipTest("kind/agent-fleet namespace not available")

    def test_control_plane_deployments_ready(self):
        for name in ("api", "ui", "postgres", "fleet-operator"):
            r = _kubectl(
                "-n",
                "agent-fleet",
                "get",
                "deploy",
                name,
                "-o",
                "jsonpath={.status.readyReplicas}",
            )
            self.assertEqual(r.returncode, 0, f"kubectl get deploy {name}: {r.stderr.strip()[:200]}")
            self.assertGreaterEqual(int(r.stdout or "0"), 1, f"{name} readyReplicas={r.stdout!r}")

    def test_crd_installed(self):
        r = _kubectl("get", "crd", "fleetsandboxes.fleet.agentfleet.io", "-o", "name")
        self.assertEqual(r.returncode, 0, f"kubectl get crd: {r.stderr.strip()[:200]}")
        self.assertIn("fleetsandboxes", r.stdout)

    def test_host_api_health(self):
        try:
            require_this_tree(_REPO, "http://127.0.0.1:8080", require_ui=True)
        except ForeignStackError as e:
            self.fail(
                f"host :8080/:3030 is not this tree: {e}. "
                "Rebuild agent-fleet-api:local and agent-fleet-ui:local "
                "(install script or ./scripts/run-every-test.sh --prepare-kind). "
                "Do not kubectl set env FLEET_GIT_SHA. SKIP_KIND=1 only if there is no cluster."
            )

    def test_host_ui(self):
        try:
            require_this_tree(_REPO, "http://127.0.0.1:8080", require_ui=True)
        except ForeignStackError as e:
            self.fail(f"host :3030 is not this tree: {e}")
        try:
            with urllib.request.urlopen("http://127.0.0.1:3030/", timeout=5) as resp:
                self.assertEqual(resp.status, 200)
            with urllib.request.urlopen(
                "http://127.0.0.1:3030/fleet-git-sha.txt", timeout=5
            ) as resp:
                self.assertEqual(resp.status, 200)
                sha = resp.read().decode().strip()
        except (urllib.error.URLError, TimeoutError) as e:
            self.fail(f"host :3030: {e}")
        want = subprocess.check_output(
            ["bash", str(_REPO / "scripts" / "tree-id.sh")],
            text=True,
            cwd=str(_REPO),
        ).strip()
        self.assertEqual(sha, want, f"UI /fleet-git-sha.txt={sha!r} != tree-id {want}")


if __name__ == "__main__":
    unittest.main(verbosity=2)
