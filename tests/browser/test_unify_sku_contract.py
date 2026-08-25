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
        self.assertIn(
            "COPY --from=builder /app/target/release/fleet-driver",
            text,
            "Kind/compose guest image must include fleet-driver",
        )
        self.assertIn(
            "COPY --from=builder /app/target/release/mock-acp-agent",
            text,
            "Kind guest must include mock-acp-agent",
        )
        self.assertNotIn(
            "COPY --from=builder /app/target/release/fleet-worker",
            text,
            "fleet-worker is not a product binary",
        )
        self.assertNotRegex(
            text,
            r"cargo build .*--bin fleet-worker",
            "image build must not produce fleet-worker",
        )

    def test_orchestrator_is_a_library_not_a_second_control_plane(self):
        toml = _read("crates", "fleet-orchestrator", "Cargo.toml")
        self.assertNotIn("[[bin]]", toml, "standalone orch binary is not in the SKU")
        self.assertFalse(
            (REPO / "crates" / "fleet-orchestrator" / "src" / "main.rs").exists(),
            "fleet-orchestrator must not have a main.rs",
        )
        self.assertFalse(
            (
                REPO
                / "crates"
                / "fleet-orchestrator"
                / "src"
                / "bin"
                / "fleet-worker.rs"
            ).exists(),
            "fleet-worker.rs must stay deleted",
        )
        just = _read("justfile")
        self.assertIn("orch: server", just)
        self.assertNotIn("cargo run -p fleet-orchestrator", just)

    def test_install_and_stage_ship_host_plus_driver_only(self):
        for rel in (
            "scripts/stage-worker-dist.sh",
            "scripts/stage-worker-dist-docker.sh",
        ):
            text = _read(*rel.split("/"))
            self.assertIn("fleet-host", text, rel)
            self.assertIn("fleet-driver", text, rel)
            self.assertNotIn("fleet-worker", text, f"{rel} still stages fleet-worker")
        unix = _read("scripts", "install-worker", "_unix_common.sh")
        self.assertIn("fleet-driver runs each job", unix)
        self.assertNotIn("fleet-worker runs each job", unix)
        self.assertNotRegex(unix, r"for bin in fleet-host fleet-worker")

    def test_control_plane_yaml_must_not_inject_guest_harness(self):
        api = _read("infra", "base", "api.yaml")
        self.assertNotIn(
            "name: FLEET_HARNESS",
            api,
            "API pod env must not pin FLEET_HARNESS; the plan LLM chooses the guest",
        )

    def test_docker_and_local_sandboxes_must_not_use_in_process_agent_job(self):
        """One guest: fleet-driver. AgentJob on the plane is a second guest."""
        docker = _read("crates", "fleet-orchestrator", "src", "sandbox", "docker.rs")
        om = _read("crates", "fleet-orchestrator", "src", "sandbox", "process_om.rs")
        proc = _read("crates", "fleet-orchestrator", "src", "sandbox", "process.rs")
        self.assertNotIn(
            "AgentJob::start",
            docker,
            "docker sandbox must isolate the guest; AgentJob on the host skips the container",
        )
        self.assertNotIn(
            "AgentJob::start",
            om,
            "local/om sandbox must exec a guest; AgentJob is not the product guest",
        )
        self.assertNotIn(
            "AgentJob::start",
            proc,
            "process sandbox guest is fleet-driver, not AgentJob",
        )

    def test_plane_docker_default_network_must_let_guests_reach_the_plane(self):
        docker = _read("crates", "fleet-orchestrator", "src", "sandbox", "docker.rs")
        # Product: default docker network is reachable (bridge). `none` drops heartbeats.
        self.assertNotRegex(
            docker,
            r'unwrap_or_else\(\|\| ["\']none["\']\.into\(\)\)',
            "DockerRuntime::from_env defaulting to network=none isolates guests from /v1",
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
