#!/usr/bin/env python3
"""Product contracts for the single-SKU runner.

The product ships one API (fleet-server), one runner (fleet-host), one guest
(fleet-driver + mock-acp-agent), plus fleet-operator for Kubernetes. A leftover
in-process AgentJob guest or a fleet-worker binary is a product hole, not a
style nit.

These tests are allowed to fail when the tree still contains a dual-guest path.
"""
from __future__ import annotations

import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]


def _read(*parts: str) -> str:
    return (REPO.joinpath(*parts)).read_text(encoding="utf-8")


class UnifySkuProductTests(unittest.TestCase):
    def test_image_ships_driver_and_mock_agent_not_fleet_worker(self):
        text = _read("Dockerfile")
        self.assertRegex(
            text,
            r"(?s)\bcp\b.*\btarget/release/fleet-driver\b.*\btarget/release/mock-acp-agent\b.*/out/",
            "builder must stage fleet-driver and mock-acp-agent to /out/",
        )
        self.assertIn(
            "COPY --from=builder /out/fleet-driver /usr/local/bin/fleet-driver",
            text,
            "Kind/compose guest image must include fleet-driver",
        )
        self.assertIn(
            "COPY --from=builder /out/mock-acp-agent /usr/local/bin/mock-acp-agent",
            text,
            "Kind guest must include mock-acp-agent",
        )
        self.assertIn(
            "COPY --from=builder /out/fleet-host /usr/local/bin/fleet-host",
            text,
            "same image is Kind host command [\"fleet-host\",\"run\"]",
        )
        self.assertNotIn(
            "/out/fleet-worker",
            text,
            "fleet-worker is not a product binary",
        )
        self.assertNotIn(
            "target/release/fleet-worker",
            text,
            "image must not stage fleet-worker",
        )
        self.assertNotRegex(
            text,
            r"(?:-p|--package|--bin)(?:=|\s+)fleet-worker\b",
            "image build must not cargo -p/--package/--bin fleet-worker",
        )

    def test_orchestrator_is_a_library_not_a_second_control_plane(self):
        orch = REPO / "crates" / "fleet-orchestrator"
        toml = _read("crates", "fleet-orchestrator", "Cargo.toml")
        self.assertNotIn("[[bin]]", toml, "standalone orch binary is not in the SKU")
        self.assertTrue(
            (orch / "src" / "lib.rs").exists(),
            "fleet-orchestrator is a library served by fleet-server",
        )
        self.assertFalse(
            (orch / "src" / "main.rs").exists(),
            "fleet-orchestrator must not have a main.rs",
        )
        self.assertEqual(
            list((orch / "src" / "bin").rglob("*.rs")),
            [],
            "src/bin/**/*.rs would cargo-autobin a second control plane",
        )
        just = _read("justfile")
        self.assertRegex(
            just,
            r"(?m)^orch:\s*server\s*$(?:\n(?![ \t])|\Z)",
            "just orch must alias server with no second-CP body",
        )
        self.assertNotRegex(
            just,
            r"cargo\s+run\b[^\n]*(?:-p|--package|--bin)(?:=|\s+)fleet-orchestrator\b",
            "justfile must not cargo-run fleet-orchestrator",
        )
        makefile = _read("crates", "fleet-orchestrator", "Makefile")
        self.assertNotRegex(
            makefile,
            r"(?m)^run:[^\n]*(?:\n[ \t][^\n]*)*\bcargo\s+run\b",
            "crate Makefile run: must not cargo-run fleet-orchestrator",
        )

if __name__ == "__main__":
    unittest.main(verbosity=2)
