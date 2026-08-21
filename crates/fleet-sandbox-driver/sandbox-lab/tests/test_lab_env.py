#!/usr/bin/env python3
"""lab_env must honor PROFILE defaults the same way the install script does."""
from __future__ import annotations

import unittest

from lab_env import env_flag, resolve_flags


class LabEnvTests(unittest.TestCase):
    def test_profile_full_without_enable_kind_is_on(self):
        flags = resolve_flags(environ={}, dotenv_text="PROFILE=full\n")
        self.assertTrue(flags["ENABLE_KIND"])
        self.assertTrue(flags["ENABLE_OLLAMA"])
        self.assertTrue(flags["ENABLE_LMSTUDIO_PROXY"])
        self.assertTrue(flags["ENABLE_GITSSH"])
        self.assertTrue(flags["ENABLE_RUNNER_GUEST_E2E"])
        self.assertTrue(env_flag("ENABLE_KIND", environ={}, dotenv_text="PROFILE=full\n"))

    def test_explicit_zero_beats_profile_full(self):
        text = "PROFILE=full\nENABLE_KIND=0\n"
        self.assertFalse(env_flag("ENABLE_KIND", environ={}, dotenv_text=text))
        self.assertTrue(env_flag("ENABLE_OLLAMA", environ={}, dotenv_text=text))

    def test_skip_kind_forces_off(self):
        text = "PROFILE=full\nSKIP_KIND=1\n"
        self.assertFalse(env_flag("ENABLE_KIND", environ={}, dotenv_text=text))

    def test_process_env_used_when_dotenv_omits_key(self):
        self.assertFalse(
            env_flag(
                "ENABLE_KIND",
                environ={"ENABLE_KIND": "0"},
                dotenv_text="PROFILE=full\n",
            )
        )

    def test_lab_profile_kind_off_ollama_on(self):
        flags = resolve_flags(environ={}, dotenv_text="PROFILE=lab\n")
        self.assertFalse(flags["ENABLE_KIND"])
        self.assertTrue(flags["ENABLE_OLLAMA"])
        self.assertTrue(flags["ENABLE_GITSSH"])
        self.assertFalse(flags["ENABLE_BROWSER_E2E"])
        self.assertTrue(flags["ENABLE_RUNNER_GUEST_E2E"])

    def test_minimal_proxy_and_gitssh_off(self):
        flags = resolve_flags(environ={}, dotenv_text="PROFILE=minimal\n")
        self.assertFalse(flags["ENABLE_LMSTUDIO_PROXY"])
        self.assertFalse(flags["ENABLE_GITSSH"])
        self.assertTrue(flags["ENABLE_OLLAMA"])
        self.assertTrue(flags["ENABLE_RUNNER_GUEST_E2E"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
