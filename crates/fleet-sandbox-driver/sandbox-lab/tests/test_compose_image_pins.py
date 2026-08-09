#!/usr/bin/env python3
"""Compose workspace/driver use stable image tags (reuse across fleet-task-*)."""
from __future__ import annotations

import unittest
from pathlib import Path

LAB = Path(__file__).resolve().parents[1]
COMPOSE = LAB / "docker-compose.yml"
INSTALL = LAB / "scripts" / "install-full-suite-deps.sh"
WS_TAG = "agent-fleet-sandbox-workspace:local"
DR_TAG = "agent-fleet-sandbox-driver:local"


class ComposeImagePinTests(unittest.TestCase):
    def test_compose_declares_stable_image_and_keeps_build(self):
        text = COMPOSE.read_text(encoding="utf-8")
        self.assertIn("COMPOSE_SANDBOX_WORKSPACE_IMAGE", text)
        self.assertIn("COMPOSE_SANDBOX_DRIVER_IMAGE", text)
        self.assertIn(WS_TAG, text)
        self.assertIn(DR_TAG, text)
        self.assertIn("build:", text)
        self.assertIn("context: ./workspace", text)
        self.assertIn("crates/fleet-sandbox-driver/sandbox-lab/driver/Dockerfile", text)
        # Defaults are the sandbox names, not the old lab-* tags.
        self.assertNotIn("agent-fleet-lab-workspace", text)
        self.assertNotIn("agent-fleet-lab-driver", text)

    def test_install_script_builds_pinned_names(self):
        text = INSTALL.read_text(encoding="utf-8")
        self.assertIn(WS_TAG, text)
        self.assertIn(DR_TAG, text)
        self.assertNotIn('docker tag "$(basename "${LAB}")-workspace"', text)


if __name__ == "__main__":
    unittest.main(verbosity=2)
