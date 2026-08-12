#!/usr/bin/env python3
"""Kind overlay / CRD kustomize builds (no live cluster required)."""
from __future__ import annotations

import shutil
import subprocess
import unittest
from pathlib import Path

LAB = Path(__file__).resolve().parents[1]
REPO = LAB.parents[2]
INFRA = REPO / "infra"


def _kustomize(path: Path) -> subprocess.CompletedProcess[str]:
    if shutil.which("kubectl"):
        return subprocess.run(
            ["kubectl", "kustomize", str(path)],
            capture_output=True,
            text=True,
        )
    if shutil.which("kustomize"):
        return subprocess.run(
            ["kustomize", "build", str(path)],
            capture_output=True,
            text=True,
        )
    return subprocess.CompletedProcess(args=[], returncode=127, stdout="", stderr="no kubectl/kustomize")


class InfraKustomizeTests(unittest.TestCase):
    def test_crds_dir_is_a_kustomization(self):
        k = INFRA / "crds" / "kustomization.yaml"
        self.assertTrue(k.is_file(), k)
        self.assertIn("fleetsandboxes.yaml", k.read_text(encoding="utf-8"))

    def test_kind_overlay_references_directories_not_crd_file(self):
        k = (INFRA / "overlays" / "kind" / "kustomization.yaml").read_text(encoding="utf-8")
        self.assertIn("../../crds", k)
        self.assertNotIn("fleetsandboxes.yaml", k)
        self.assertIn("api-nodeport-netpol.yaml", k)
        self.assertIn("nodeport.yaml", k)

    def test_k3d_overlay_builds(self):
        k = (INFRA / "overlays" / "k3d" / "kustomization.yaml").read_text(encoding="utf-8")
        self.assertIn("../../crds", k)
        self.assertIn("nodeport.yaml", k)
        r = _kustomize(INFRA / "overlays" / "k3d")
        self.assertNotEqual(
            r.returncode,
            127,
            "kubectl/kustomize not on PATH (run install-full-suite-deps.sh)",
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        out = r.stdout
        self.assertIn("kind: CustomResourceDefinition", out)
        self.assertIn("name: api-node", out)
        self.assertIn("nodePort: 30080", out)
        self.assertIn("nodePort: 30303", out)

    def test_kind_overlay_builds(self):
        r = _kustomize(INFRA / "overlays" / "kind")
        self.assertNotEqual(
            r.returncode,
            127,
            "kubectl/kustomize not on PATH (run install-full-suite-deps.sh)",
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        out = r.stdout
        self.assertIn("kind: Namespace", out)
        self.assertIn("kind: CustomResourceDefinition", out)
        self.assertIn("fleetsandboxes.fleet.agentfleet.io", out)
        self.assertIn("name: api-node", out)
        self.assertIn("name: ui-node", out)
        self.assertIn("name: api-ingress-nodeport", out)
        self.assertIn("nodePort: 30080", out)
        self.assertIn("nodePort: 30303", out)

    def test_base_infra_builds(self):
        r = _kustomize(INFRA)
        self.assertNotEqual(
            r.returncode,
            127,
            "kubectl/kustomize not on PATH (run install-full-suite-deps.sh)",
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("kind: CustomResourceDefinition", r.stdout)

    def test_api_nodeport_netpol_allows_port_8080_without_from(self):
        p = INFRA / "overlays" / "kind" / "api-nodeport-netpol.yaml"
        text = p.read_text(encoding="utf-8")
        self.assertIn("api-ingress-nodeport", text)
        self.assertIn("port: 8080", text)
        # No from: block — host/NodePort traffic is not an in-ns pod.
        self.assertNotIn("from:", text)


if __name__ == "__main__":
    unittest.main(verbosity=2)
