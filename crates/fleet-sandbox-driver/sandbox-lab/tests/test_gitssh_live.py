#!/usr/bin/env python3
"""Live clone against install-script git-ssh (:2222). Skips if server/key missing."""
from __future__ import annotations

import importlib.util
import os
import shutil
import socket
import subprocess
import tempfile
import unittest
from pathlib import Path

from lab_env import env_flag

LAB = Path(__file__).resolve().parents[1]
REPO = LAB.parents[2]
AGENT = LAB / "workspace" / "workspace_agent.py"
KEY = REPO / ".tmp" / "ssh" / "id_ed25519"
HELLO = "ssh://git@127.0.0.1:2222/srv/git/hello.git"


def _port_open(host: str, port: int) -> bool:
    try:
        with socket.create_connection((host, port), timeout=1):
            return True
    except OSError:
        return False


def _load_agent():
    spec = importlib.util.spec_from_file_location("wa_gitssh", AGENT)
    wa = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(wa)
    return wa


def _clone_env(home: Path) -> dict[str, str]:
    """HOME for git_ssh_env only — do not mutate process HOME (kind kubeconfig)."""
    home.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env["HOME"] = str(home)
    return env


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

    def setUp(self):
        # Kind smoke (later in discover) uses $HOME/.kube/config. A leaked HOME
        # makes kubectl default to :8080 (fleet-server), not the kind API.
        self._saved = {k: os.environ.get(k) for k in ("HOME", "GIT_SSH_KEY_PATH")}
        self.addCleanup(self._restore_env)

    def _restore_env(self):
        for k, v in self._saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v

    def test_workspace_agent_clones_local_gitssh(self):
        wa = _load_agent()
        tmp = Path(tempfile.mkdtemp(prefix="fleet-gitssh-test-"))
        dest = tmp / "jobs" / "j1" / "repo"
        env = _clone_env(tmp / "home")
        env["GIT_SSH_KEY_PATH"] = str(KEY)
        try:
            wa.git_clone(HELLO, "main", dest, env)
            readme = (dest / "README").read_text(encoding="utf-8")
            self.assertIn("hello-from-local-git-ssh", readme)
            self.assertEqual(os.environ.get("HOME"), self._saved["HOME"])
            self.assertNotEqual(Path.home(), tmp / "home")
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_workspace_agent_ssh_clone_rejects_wrong_key(self):
        # Unauthorized key must fail even if SSH_AUTH_SOCK holds the dummy.
        wa = _load_agent()
        tmp = Path(tempfile.mkdtemp(prefix="fleet-gitssh-wrong-"))
        dest = tmp / "jobs" / "j1" / "repo"
        bad_key = tmp / "id_ed25519"
        try:
            subprocess.run(
                ["ssh-keygen", "-t", "ed25519", "-f", str(bad_key), "-N", "", "-q"],
                check=True,
                capture_output=True,
            )
            env = _clone_env(tmp / "home")
            env["GIT_SSH_KEY_PATH"] = str(bad_key)
            with self.assertRaises(RuntimeError):
                wa.git_clone(HELLO, "main", dest, env)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    unittest.main(verbosity=2)
