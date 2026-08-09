#!/usr/bin/env python3
"""Live kind cluster smoke. Skips when cluster is not installed."""
from __future__ import annotations

import os
import shutil
import subprocess
import urllib.error
import urllib.request
import unittest
from pathlib import Path


def _http_ok(url: str) -> bool:
    try:
        with urllib.request.urlopen(url, timeout=3) as resp:
            return 200 <= resp.status < 300
    except (urllib.error.URLError, TimeoutError, OSError):
        return False


def _kubectl(*args: str) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    kube = Path.home() / ".kube" / "config"
    if kube.is_file():
        env.setdefault("KUBECONFIG", str(kube))
    return subprocess.run(
        ["kubectl", *args],
        capture_output=True,
        text=True,
        timeout=30,
        env=env,
    )


def _cluster() -> bool:
    # Host extraPortMappings are the install contract; don't require kubectl here.
    return _http_ok("http://127.0.0.1:8080/health")


@unittest.skipUnless(_cluster(), "kind/agent-fleet namespace not available")
class KindSmokeTests(unittest.TestCase):
    def test_control_plane_deployments_ready(self):
        if not shutil.which("kubectl"):
            self.skipTest("kubectl not on PATH")
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
            if r.returncode != 0:
                self.skipTest(f"kubectl get deploy {name}: {r.stderr.strip()[:200]}")
            self.assertGreaterEqual(int(r.stdout or "0"), 1, f"{name} readyReplicas={r.stdout!r}")

    def test_crd_installed(self):
        if not shutil.which("kubectl"):
            self.skipTest("kubectl not on PATH")
        r = _kubectl("get", "crd", "fleetsandboxes.fleet.agentfleet.io", "-o", "name")
        if r.returncode != 0:
            self.skipTest(f"kubectl get crd: {r.stderr.strip()[:200]}")
        self.assertIn("fleetsandboxes", r.stdout)

    def test_host_api_health(self):
        try:
            with urllib.request.urlopen("http://127.0.0.1:8080/health", timeout=5) as resp:
                body = resp.read().decode()
                self.assertEqual(resp.status, 200)
                self.assertIn("fleet-server", body)
        except (urllib.error.URLError, TimeoutError) as e:
            self.fail(f"host :8080 /health: {e}")

    def test_host_ui(self):
        try:
            with urllib.request.urlopen("http://127.0.0.1:3030/", timeout=5) as resp:
                self.assertEqual(resp.status, 200)
        except (urllib.error.URLError, TimeoutError) as e:
            self.fail(f"host :3030: {e}")


if __name__ == "__main__":
    unittest.main(verbosity=2)
