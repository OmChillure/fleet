#!/usr/bin/env python3
"""Unit tests for publish-job env overlay (no Redis/Docker)."""
from __future__ import annotations

import importlib.util
import os
import unittest
from pathlib import Path

LAB = Path(__file__).resolve().parents[1]
REPO = LAB.parents[2]
SCRIPTS = LAB / "scripts"
SPEC = importlib.util.spec_from_file_location("publish_lab_job", SCRIPTS / "_publish_lab_job.py")
mod = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(mod)


class PublishOverlayTests(unittest.TestCase):
    def setUp(self):
        self._saved = {k: os.environ.get(k) for k in list(os.environ) if k.startswith("OPENCODE_") or k in ("MODEL", "HARNESS", "FLEET_FORCE_HARNESS")}
        for k in ("OPENCODE_BASE_URL", "OPENCODE_PROVIDER_ID", "OPENCODE_PROVIDER_NAME",
                  "OPENCODE_API_KEY", "OPENCODE_PROVIDER_NPM", "MODEL", "HARNESS", "FLEET_FORCE_HARNESS"):
            os.environ.pop(k, None)

    def tearDown(self):
        for k in ("OPENCODE_BASE_URL", "OPENCODE_PROVIDER_ID", "OPENCODE_PROVIDER_NAME",
                  "OPENCODE_API_KEY", "OPENCODE_PROVIDER_NPM", "MODEL", "HARNESS", "FLEET_FORCE_HARNESS"):
            os.environ.pop(k, None)
        for k, v in self._saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v

    def test_overlay_rewrites_lmstudio_job_to_ollama(self):
        os.environ["OPENCODE_BASE_URL"] = "http://host.docker.internal:11434/v1"
        os.environ["OPENCODE_PROVIDER_ID"] = "ollama"
        os.environ["OPENCODE_PROVIDER_NAME"] = "Ollama (local)"
        os.environ["OPENCODE_API_KEY"] = "ollama"
        os.environ["MODEL"] = "ollama/llama3.2:1b"
        job = {
            "harness": "opencode",
            "model": "lmstudio/google/gemma-4-e4b",
            "provider": {
                "id": "lmstudio",
                "name": "LM Studio (local)",
                "base_url": "http://host.docker.internal:1234/v1",
                "api_key": "lm-studio",
                "models": {"google/gemma-4-e4b": {"name": "Gemma"}},
            },
        }
        out = mod.apply_env_overlay(job)
        self.assertEqual(out["model"], "ollama/llama3.2:1b")
        self.assertEqual(out["provider"]["id"], "ollama")
        self.assertEqual(out["provider"]["base_url"], "http://host.docker.internal:11434/v1")
        self.assertEqual(out["provider"]["api_key"], "ollama")
        self.assertIn("llama3.2:1b", out["provider"]["models"])

    def test_job_harness_wins_over_env(self):
        os.environ["HARNESS"] = "opencode"
        job = {"harness": "pi", "provider": {}}
        out = mod.apply_env_overlay(job)
        self.assertEqual(out["harness"], "pi")

    def test_force_harness(self):
        os.environ["HARNESS"] = "opencode"
        os.environ["FLEET_FORCE_HARNESS"] = "1"
        job = {"harness": "pi", "provider": {}}
        out = mod.apply_env_overlay(job)
        self.assertEqual(out["harness"], "opencode")


if __name__ == "__main__":
    unittest.main(verbosity=2)
