#!/usr/bin/env python3
"""Host services the install script is supposed to leave running.

Postgres :5433 is required (fail if down). Ollama :11434 and the OpenAI-compat
proxy :1234 skip only when the matching ENABLE_* flag is off. When the flag is
on, a down port is a fail.
"""
from __future__ import annotations

import json
import shutil
import socket
import subprocess
import urllib.request
import unittest

from lab_env import env_flag


def _port(port: int) -> bool:
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=1):
            return True
    except OSError:
        return False


class HostServiceTests(unittest.TestCase):
    def test_ollama_or_skip(self):
        if not _port(11434):
            if env_flag("ENABLE_OLLAMA"):
                self.fail("ENABLE_OLLAMA=1 but ollama :11434 is not listening")
            self.skipTest("ollama :11434 not listening")
        with urllib.request.urlopen("http://127.0.0.1:11434/api/tags", timeout=3) as r:
            data = json.loads(r.read().decode())
        names = [m.get("name") for m in data.get("models", [])]
        self.assertTrue(names, "ollama has no models")

    def test_openai_compat_proxy_or_skip(self):
        if not _port(1234):
            if env_flag("ENABLE_LMSTUDIO_PROXY"):
                self.fail("ENABLE_LMSTUDIO_PROXY=1 but :1234 proxy is not listening")
            self.skipTest(":1234 proxy not listening")
        with urllib.request.urlopen("http://127.0.0.1:1234/v1/models", timeout=3) as r:
            data = json.loads(r.read().decode())
        self.assertIn("data", data)

    def test_host_postgres(self):
        self.assertTrue(
            _port(5433),
            "host postgres :5433 not listening (install-full-suite-deps.sh leaves it up)",
        )
        pg = shutil.which("pg_isready")
        if pg:
            r = subprocess.run(
                [pg, "-h", "127.0.0.1", "-p", "5433"],
                capture_output=True,
                text=True,
                timeout=5,
            )
            self.assertEqual(r.returncode, 0, (r.stdout or "") + (r.stderr or ""))
        else:
            with socket.create_connection(("127.0.0.1", 5433), timeout=2) as sock:
                peer = sock.getpeername()
            self.assertEqual(peer[1], 5433)


if __name__ == "__main__":
    unittest.main(verbosity=2)
