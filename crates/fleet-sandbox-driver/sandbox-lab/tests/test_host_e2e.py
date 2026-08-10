#!/usr/bin/env python3
"""Host e2e: workspace_agent + signed provision + fleet-wsexec (no Docker/LLM).

Uses only tempdirs and a local git repo. No homedir, secrets, or machine paths.
"""
from __future__ import annotations

import hashlib
import hmac
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

SECRET = "e2e-handshake-secret"
LAB = Path(__file__).resolve().parents[1]
AGENT = LAB / "workspace" / "workspace_agent.py"


def _sign(req: dict) -> dict:
    body = {k: v for k, v in req.items() if k != "hmac"}
    raw = json.dumps(body, sort_keys=True, separators=(",", ":")).encode()
    body["hmac"] = hmac.new(SECRET.encode(), raw, hashlib.sha256).hexdigest()
    return body


def _wait_file(path: Path, timeout: float = 20.0) -> dict | None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if path.is_file():
            try:
                return json.loads(path.read_text(encoding="utf-8"))
            except json.JSONDecodeError:
                time.sleep(0.05)
                continue
        time.sleep(0.05)
    return None


class HostE2ETests(unittest.TestCase):
    """HMAC provision, fleet-wsexec tools, unsigned-request rejection."""

    def test_agent_provision_and_wsexec(self):
        self.assertTrue(AGENT.is_file(), f"missing workspace agent at {AGENT}")

        tmp = Path(tempfile.mkdtemp(prefix="fleet-e2e-"))
        fleet = tmp / ".fleet"
        jobs = tmp / "jobs"
        bindir = tmp / "bin"
        src = tmp / "src-repo"
        fleet.mkdir()
        jobs.mkdir()
        bindir.mkdir()
        src.mkdir()
        (src / "README").write_text("hello-from-src\n", encoding="utf-8")
        subprocess.run(["git", "init", "-q", str(src)], check=True)
        subprocess.run(
            ["git", "-C", str(src), "config", "user.email", "e2e@example.test"],
            check=True,
        )
        subprocess.run(
            ["git", "-C", str(src), "config", "user.name", "e2e"],
            check=True,
        )
        subprocess.run(["git", "-C", str(src), "add", "README"], check=True)
        subprocess.run(
            ["git", "-C", str(src), "commit", "-q", "-m", "init"],
            check=True,
        )
        subprocess.run(["git", "-C", str(src), "branch", "-M", "main"], check=True)

        env = os.environ.copy()
        env.update(
            {
                "FLEET_DIR": str(fleet),
                "FLEET_JOBS_ROOT": str(jobs),
                "FLEET_BIN_DIR": str(bindir),
                "FLEET_HANDSHAKE_SECRET": SECRET,
                "FLEET_ALLOW_RAW_EXEC": "0",
                "FLEET_WORKSPACE_POLL_SEC": "0.05",
                "FLEET_UID": str(os.getuid()),
                "FLEET_GID": str(os.getgid()),
                "PYTHONUNBUFFERED": "1",
            }
        )
        log_path = tmp / "agent.log"
        logf = open(log_path, "w", encoding="utf-8")
        proc = subprocess.Popen(
            [sys.executable, str(AGENT)],
            env=env,
            stdout=logf,
            stderr=subprocess.STDOUT,
        )
        try:
            alive = fleet / "agent.alive"
            for _ in range(100):
                if alive.is_file() and (bindir / "fleet-wsexec").is_file():
                    break
                time.sleep(0.05)
            else:
                tail = log_path.read_text(encoding="utf-8")[-2000:]
                self.fail(f"agent.alive or fleet-wsexec missing\n{tail}")

            wsexec = bindir / "fleet-wsexec"
            st = wsexec.stat()
            self.assertFalse(
                st.st_mode & 0o022,
                f"fleet-wsexec is group/other-writable: mode={oct(st.st_mode)}",
            )

            job_id = "e2e-job-1"
            work_dir = jobs / job_id / "repo"
            req_id = "prov-1"
            provision = _sign(
                {
                    "job_id": job_id,
                    "action": "provision",
                    "req_id": req_id,
                    "repo_url": str(src),
                    "git_ref": "main",
                    "work_dir": str(work_dir),
                    "commands": ["mkdir -p docs", "echo provisioned > docs/marker.txt"],
                    "smoke_commands": ["test -f README"],
                    "env": {},
                    "tools": {
                        "hello": {"argv": ["echo", "e2e-hello"], "description": "echo"},
                        "show_readme": {
                            "argv": ["cat", "README"],
                            "description": "cat",
                        },
                        "show_marker": {
                            "argv": ["cat", "docs/marker.txt"],
                            "description": "marker",
                        },
                    },
                }
            )
            (fleet / f"request-{req_id}.json").write_text(
                json.dumps(provision), encoding="utf-8"
            )
            result = _wait_file(fleet / f"result-{req_id}.json", 30)
            self.assertIsNotNone(result, "provision timed out")
            assert result is not None
            self.assertTrue(
                result.get("ok"),
                f"provision failed: {result.get('error')}",
            )
            self.assertTrue(
                (work_dir / "README").is_file(),
                "clone did not produce README",
            )
            self.assertTrue(
                (work_dir / "docs" / "marker.txt").is_file(),
                "provision command did not write docs/marker.txt",
            )

            child_env = env.copy()
            child_env["FLEET_WORK_DIR"] = str(work_dir)
            child_env["FLEET_JOB_ID"] = job_id
            child_env.pop("FLEET_HANDSHAKE_SECRET", None)

            p = subprocess.run(
                [str(wsexec), "hello"],
                cwd=str(work_dir),
                env=child_env,
                capture_output=True,
                text=True,
                timeout=20,
            )
            self.assertEqual(p.returncode, 0, p.stderr)
            self.assertIn("e2e-hello", p.stdout)

            p = subprocess.run(
                [str(wsexec), "show_marker"],
                cwd=str(work_dir),
                env=child_env,
                capture_output=True,
                text=True,
                timeout=20,
            )
            self.assertEqual(p.returncode, 0, p.stderr)
            self.assertIn("provisioned", p.stdout)

            p = subprocess.run(
                [str(wsexec), "not_a_tool"],
                cwd=str(work_dir),
                env=child_env,
                capture_output=True,
                text=True,
                timeout=20,
            )
            self.assertNotEqual(p.returncode, 0, "unknown tool unexpectedly succeeded")

            tools_path = work_dir / ".fleet-tools.json"
            try:
                tools_path.write_text(
                    json.dumps({"hello": {"argv": ["echo", "from-file-mutation"]}}),
                    encoding="utf-8",
                )
            except PermissionError:
                # 0444 tools file: in-place rewrite denied is fine; unlink+replace may work
                tools_path.unlink()
                tools_path.write_text(
                    json.dumps({"hello": {"argv": ["echo", "from-file-mutation"]}}),
                    encoding="utf-8",
                )

            p = subprocess.run(
                [str(wsexec), "hello"],
                cwd=str(work_dir),
                env=child_env,
                capture_output=True,
                text=True,
                timeout=20,
            )
            self.assertIn("e2e-hello", p.stdout)
            self.assertNotIn(
                "from-file-mutation",
                p.stdout,
                f"tools file mutation leaked: out={p.stdout!r}",
            )

            bad_id = "bad-argv"
            (fleet / f"request-{bad_id}.json").write_text(
                json.dumps(
                    {
                        "job_id": job_id,
                        "action": "tool",
                        "req_id": bad_id,
                        "tool": "hello",
                        "cwd": str(work_dir),
                        "argv": ["echo", "pwned"],
                    }
                ),
                encoding="utf-8",
            )
            bad = _wait_file(fleet / f"result-{bad_id}.json")
            self.assertIsNotNone(bad, "unsigned argv override: no result")
            assert bad is not None
            self.assertFalse(bad.get("ok"), f"unsigned argv override not rejected: {bad}")
            self.assertIn("argv", (bad.get("error") or "").lower())

            fake_id = "fake-prov"
            (fleet / f"request-{fake_id}.json").write_text(
                json.dumps(
                    {
                        "job_id": "evil",
                        "action": "provision",
                        "req_id": fake_id,
                        "repo_url": str(src),
                        "git_ref": "main",
                        "work_dir": str(jobs / "evil" / "repo"),
                        "commands": ["echo pwned"],
                        "smoke_commands": [],
                        "env": {},
                        "tools": {},
                    }
                ),
                encoding="utf-8",
            )
            fake = _wait_file(fleet / f"result-{fake_id}.json")
            self.assertIsNotNone(fake, "unsigned provision: no result")
            assert fake is not None
            self.assertFalse(fake.get("ok"), f"unsigned provision accepted: {fake}")

            p = subprocess.run(
                [str(wsexec), "show_readme"],
                cwd=str(work_dir),
                env=child_env,
                capture_output=True,
                text=True,
                timeout=20,
            )
            self.assertEqual(p.returncode, 0, p.stderr)
            self.assertIn("hello-from-src", p.stdout)
        finally:
            proc.send_signal(signal.SIGTERM)
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
            logf.close()
            shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    unittest.main(verbosity=2)
