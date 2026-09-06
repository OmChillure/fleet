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


def _v1_model_ids(port: int) -> list[str]:
    with urllib.request.urlopen(f"http://127.0.0.1:{port}/v1/models", timeout=3) as r:
        payload = json.loads(r.read().decode())
    rows = payload.get("data") if isinstance(payload, dict) else None
    if not isinstance(rows, list):
        return []
    return [m["id"] for m in rows if isinstance(m, dict) and m.get("id")]


def _listen_addrs(port: int) -> list[str]:
    ss = shutil.which("ss")
    if ss is None:
        return []
    r = subprocess.run([ss, "-ltn"], capture_output=True, text=True, timeout=5)
    return [
        tok
        for tok in (r.stdout or "").split()
        if tok.endswith(f":{port}") or tok.endswith(f"]:{port}")
    ]


def _listen_is_public(addrs: list[str]) -> bool:
    return any(a.startswith(("0.0.0.0:", "*:", "[::]:")) for a in addrs)


class HostServiceTests(unittest.TestCase):
    def _require_openai_compat_proxy(self) -> None:
        if not _port(1234):
            if env_flag("ENABLE_LMSTUDIO_PROXY"):
                self.fail("ENABLE_LMSTUDIO_PROXY=1 but :1234 proxy is not listening")
            self.skipTest(":1234 proxy not listening")

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
        self._require_openai_compat_proxy()
        ids = _v1_model_ids(1234)
        self.assertTrue(ids, ":1234 /v1/models has no data[].id")

    def test_openai_compat_proxy_v1_models_ids_match_ollama_when_enabled(self):
        self._require_openai_compat_proxy()
        if not _port(11434):
            self.fail(
                "ENABLE_LMSTUDIO_PROXY leftover requires ollama :11434; "
                ":1234 is up but :11434 is not listening"
            )
        proxy_ids = set(_v1_model_ids(1234))
        ollama_ids = set(_v1_model_ids(11434))
        self.assertTrue(
            proxy_ids & ollama_ids,
            f":1234 /v1/models ids {sorted(proxy_ids)} do not intersect "
            f":11434 /v1/models {sorted(ollama_ids)}",
        )

    def test_openai_compat_proxy_listens_on_0_0_0_0_when_enabled(self):
        self._require_openai_compat_proxy()
        ss = shutil.which("ss")
        self.assertIsNotNone(
            ss,
            "ss missing; cannot verify :1234 is bound on 0.0.0.0 "
            "(install bind=0.0.0.0 so containers can use host.docker.internal)",
        )
        addrs = _listen_addrs(1234)
        self.assertTrue(
            _listen_is_public(addrs),
            f":1234 listen {addrs or 'none'} is not 0.0.0.0/*; "
            "containers cannot use host.docker.internal:1234",
        )

    def test_host_postgres(self):
        self.assertTrue(
            _port(5433),
            "host postgres :5433 not listening (install-full-suite-deps.sh leaves it up)",
        )
        pg = shutil.which("pg_isready")
        self.assertIsNotNone(
            pg,
            "pg_isready missing; install leaves host postgres ready via "
            "pg_isready -h 127.0.0.1 -p 5433 -U fleet -d fleet",
        )
        r = subprocess.run(
            [pg, "-h", "127.0.0.1", "-p", "5433", "-U", "fleet", "-d", "fleet"],
            capture_output=True,
            text=True,
            timeout=5,
        )
        self.assertEqual(r.returncode, 0, (r.stdout or "") + (r.stderr or ""))


if __name__ == "__main__":
    unittest.main(verbosity=2)
