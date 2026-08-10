#!/usr/bin/env python3
"""Live clone against install-script git-ssh (:2222). Skips if server/key missing."""
from __future__ import annotations

import importlib.util
import os
import shutil
import socket
import tempfile
import unittest
from pathlib import Path

from lab_env import env_flag

LAB = Path(__file__).resolve().parents[1]
REPO = LAB.parents[2]
AGENT = LAB / "workspace" / "workspace_agent.py"
KEY = REPO / ".tmp" / "ssh" / "id_ed25519"


def _port_open(host: str, port: int) -> bool:
    try:
        with socket.create_connection((host, port), timeout=1):
            return True
    except OSError:
        return False


class GitSshLiveTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        key_ok = KEY.is_file()
        port_ok = _port_open("127.0.0.1", 2222)
        if key_ok and port_ok:
            return
        if env_flag("ENABLE_GITSSH"):
            missing = []
            if not key_ok:
                missing.append("dummy SSH key .tmp/ssh/id_ed25519")
            if not port_ok:
                missing.append("git-ssh :2222")
            raise AssertionError(
                "ENABLE_GITSSH=1 but missing: " + ", ".join(missing)
            )
        if not key_ok:
            raise unittest.SkipTest("dummy SSH key not installed (.tmp/ssh)")
        raise unittest.SkipTest("git-ssh :2222 not listening")

    def test_workspace_agent_clones_local_gitssh(self):
        spec = importlib.util.spec_from_file_location("wa_gitssh", AGENT)
        wa = importlib.util.module_from_spec(spec)
        assert spec.loader is not None
        spec.loader.exec_module(wa)
        tmp = Path(tempfile.mkdtemp(prefix="fleet-gitssh-test-"))
        old_home = os.environ.get("HOME")
        os.environ["GIT_SSH_KEY_PATH"] = str(KEY)
        os.environ["HOME"] = str(tmp / "home")
        (tmp / "home").mkdir()
        dest = tmp / "jobs" / "j1" / "repo"
        try:
            wa.git_clone(
                "ssh://git@127.0.0.1:2222/srv/git/hello.git",
                "main",
                dest,
                os.environ.copy(),
            )
            readme = (dest / "README").read_text(encoding="utf-8")
            self.assertIn("hello-from-local-git-ssh", readme)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)
            os.environ.pop("GIT_SSH_KEY_PATH", None)
            if old_home is None:
                os.environ.pop("HOME", None)
            else:
                os.environ["HOME"] = old_home


if __name__ == "__main__":
    unittest.main(verbosity=2)
