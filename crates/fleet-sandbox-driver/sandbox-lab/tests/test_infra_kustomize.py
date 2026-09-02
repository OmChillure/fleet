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


def _role_named(built: str, name: str) -> str | None:
    """Return the YAML document for `kind: Role` with this metadata.name."""
    for doc in built.split("\n---\n"):
        # RoleBinding roleRef also says `kind: Role` — skip those documents.
        if "kind: RoleBinding" in doc:
            continue
        if "\nkind: Role\n" not in f"\n{doc}\n":
            continue
        if f"name: {name}" in doc:
            return doc
    return None


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
        self._assert_in_cluster_k8s_path(out)

    def test_base_infra_builds(self):
        r = _kustomize(INFRA)
        self.assertNotEqual(
            r.returncode,
            127,
            "kubectl/kustomize not on PATH (run install-full-suite-deps.sh)",
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("kind: CustomResourceDefinition", r.stdout)
        self._assert_in_cluster_k8s_path(r.stdout)

    def test_k3d_overlay_has_in_cluster_k8s_path(self):
        r = _kustomize(INFRA / "overlays" / "k3d")
        self.assertNotEqual(
            r.returncode,
            127,
            "kubectl/kustomize not on PATH (run install-full-suite-deps.sh)",
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        self._assert_in_cluster_k8s_path(r.stdout)

    def _assert_in_cluster_k8s_path(self, out: str) -> None:
        self.assertIn("name: fleet-api", out)
        self.assertIn("serviceAccountName: fleet-api", out)
        self.assertNotIn("SANDBOX_BACKEND", out)
        self.assertIn(
            "http://api.agent-fleet.svc.cluster.local:8080",
            out,
        )
        api_role = _role_named(out, "fleet-api")
        self.assertIsNotNone(api_role, "fleet-api Role missing from built YAML")
        self.assertNotIn("fleetsandboxes", api_role)
        self.assertNotIn("pods", api_role)
        self.assertNotIn("networkpolicies", api_role)
        self.assertIn("name: fleet-host", out)
        self.assertIn("serviceAccountName: fleet-host", out)
        self.assertIn("name: zenohd", out)
        self.assertIn("FLEET_HARNESS", out)
        self.assertRegex(out, r"FLEET_HARNESS\n\s+value: mock")
        self.assertIn("fleet-driver", out)
        self.assertIn("FLEET_BOOTSTRAP_SECRET", out)
        host_role = _role_named(out, "fleet-host")
        self.assertIsNotNone(host_role, "fleet-host Role missing from built YAML")
        self.assertIn("fleetsandboxes", host_role)
        self.assertNotIn("pods", host_role)
        self.assertNotIn("networkpolicies", host_role)

    def test_api_nodeport_netpol_allows_port_8080_without_from(self):
        p = INFRA / "overlays" / "kind" / "api-nodeport-netpol.yaml"
        text = p.read_text(encoding="utf-8")
        self.assertIn("api-ingress-nodeport", text)
        self.assertIn("port: 8080", text)
        # No from: block — host/NodePort traffic is not an in-ns pod.
        self.assertNotIn("from:", text)


if __name__ == "__main__":
    unittest.main(verbosity=2)
