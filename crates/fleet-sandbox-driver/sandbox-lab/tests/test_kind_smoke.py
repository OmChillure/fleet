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


@unittest.skipUnless(_cluster(), "kind/agent-fleet namespace not available")
class KindSmokeTests(unittest.TestCase):
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
