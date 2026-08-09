#!/usr/bin/env python3
"""Host services the install script is supposed to leave running. Soft-skip."""
from __future__ import annotations

import json
import socket
import urllib.request
import unittest


def _port(port: int) -> bool:
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=1):
            return True
    except OSError:
        return False


class HostServiceTests(unittest.TestCase):
    def test_ollama_or_skip(self):
        if not _port(11434):
            self.skipTest("ollama :11434 not listening")
        with urllib.request.urlopen("http://127.0.0.1:11434/api/tags", timeout=3) as r:
            data = json.loads(r.read().decode())
        names = [m.get("name") for m in data.get("models", [])]
        self.assertTrue(names, "ollama has no models")

    def test_openai_compat_proxy_or_skip(self):
        if not _port(1234):
            self.skipTest(":1234 proxy not listening")
        with urllib.request.urlopen("http://127.0.0.1:1234/v1/models", timeout=3) as r:
            data = json.loads(r.read().decode())
        self.assertIn("data", data)

    def test_host_postgres_or_skip(self):
        if not _port(5433):
            self.skipTest("host postgres :5433 not listening")


if __name__ == "__main__":
    unittest.main(verbosity=2)
