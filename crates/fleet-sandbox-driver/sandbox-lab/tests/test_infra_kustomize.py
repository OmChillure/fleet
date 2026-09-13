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


def _service_named(built: str, name: str) -> str | None:
    """Return the YAML document for `kind: Service` with this metadata.name."""
    for doc in built.split("\n---\n"):
        if "\nkind: Service\n" not in f"\n{doc}\n":
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
    def test_k3d_overlay_builds(self):
        r = _kustomize(INFRA / "overlays" / "k3d")
        self.assertNotEqual(
            r.returncode,
            127,
            "kubectl/kustomize not on PATH (run install-full-suite-deps.sh)",
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        out = r.stdout
        self.assertIn("fleetsandboxes.fleet.agentfleet.io", out)
        self.assertIn("name: api-node", out)
        self.assertIn("name: ui-node", out)
        self.assertIn("nodePort: 30080", out)
        self.assertIn("nodePort: 30303", out)

    def test_k3d_yaml_loadbalancer_maps_host_8080_3030_to_overlay_nodeports(self):
        text = (INFRA / "k3d.yaml").read_text(encoding="utf-8")
        self.assertIn("8080:30080", text)
        self.assertIn("3030:30303", text)

    def test_kind_overlay_builds(self):
        r = _kustomize(INFRA / "overlays" / "kind")
        self.assertNotEqual(
            r.returncode,
            127,
            "kubectl/kustomize not on PATH (run install-full-suite-deps.sh)",
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        out = r.stdout
        self.assertIn("fleetsandboxes.fleet.agentfleet.io", out)
        self.assertIn("name: api-node", out)
        self.assertIn("name: ui-node", out)
        self.assertIn("name: api-ingress-nodeport", out)
        self.assertIn("nodePort: 30080", out)
        self.assertIn("nodePort: 30303", out)

    def test_kind_yaml_extra_port_mappings_match_overlay_nodeports(self):
        kind = (INFRA / "kind.yaml").read_text(encoding="utf-8")
        nodeport = (INFRA / "overlays" / "kind" / "nodeport.yaml").read_text(
            encoding="utf-8"
        )
        self.assertIn("containerPort: 30080", kind)
        self.assertIn("hostPort: 8080", kind)
        self.assertIn("containerPort: 30303", kind)
        self.assertIn("hostPort: 3030", kind)
        self.assertIn("nodePort: 30080", nodeport)
        self.assertIn("nodePort: 30303", nodeport)

    def test_kind_overlay_nodeport_services_select_api_and_ui(self):
        r = _kustomize(INFRA / "overlays" / "kind")
        self.assertNotEqual(
            r.returncode,
            127,
            "kubectl/kustomize not on PATH (run install-full-suite-deps.sh)",
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        api = _service_named(r.stdout, "api-node")
        self.assertIsNotNone(api, "api-node Service missing from built YAML")
        self.assertIn("type: NodePort", api)
        self.assertRegex(
            api, r"selector:\n\s+app\.kubernetes\.io/name: fleet-api"
        )
        ui = _service_named(r.stdout, "ui-node")
        self.assertIsNotNone(ui, "ui-node Service missing from built YAML")
        self.assertIn("type: NodePort", ui)
        self.assertRegex(
            ui, r"selector:\n\s+app\.kubernetes\.io/name: fleet-ui"
        )

    def test_kind_overlay_kustomization_includes_base(self):
        text = (INFRA / "overlays" / "kind" / "kustomization.yaml").read_text(
            encoding="utf-8"
        )
        self.assertIn("  - ../../base", text)

    def test_base_infra_builds(self):
        r = _kustomize(INFRA)
        self.assertNotEqual(
            r.returncode,
            127,
            "kubectl/kustomize not on PATH (run install-full-suite-deps.sh)",
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("kind: CustomResourceDefinition", r.stdout)
        self.assertNotIn("nodePort", r.stdout)

    def test_make_apply_and_uninstall_use_infra_root_kustomize(self):
        text = (INFRA / "Makefile").read_text(encoding="utf-8")
        self.assertIn("KUSTOMIZE ?= $(ROOT)/infra\n", text)
        self.assertIn("\napply:\n\tkubectl apply -k $(KUSTOMIZE)\n", text)
        self.assertIn("\nuninstall:\n\tkubectl delete -k $(KUSTOMIZE)", text)


if __name__ == "__main__":
    unittest.main(verbosity=2)
