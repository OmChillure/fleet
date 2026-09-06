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


def _sign_with(req: dict, secret: str) -> dict:
    body = {k: v for k, v in req.items() if k != "hmac"}
    raw = json.dumps(body, sort_keys=True, separators=(",", ":")).encode()
    body["hmac"] = hmac.new(secret.encode(), raw, hashlib.sha256).hexdigest()
    return body


def _sign(req: dict) -> dict:
    return _sign_with(req, SECRET)


def _submit_request(fleet: Path, req: dict, timeout: float = 20.0) -> dict | None:
    req_id = req["req_id"]
    (fleet / f"request-{req_id}.json").write_text(
        json.dumps(req), encoding="utf-8"
    )
    return _wait_file(fleet / f"result-{req_id}.json", timeout)


def _hmac_provision(
    job_id: str, req_id: str, repo_url: str, work_dir: Path
) -> dict:
    return {
        "job_id": job_id,
        "action": "provision",
        "req_id": req_id,
        "repo_url": repo_url,
        "git_ref": "main",
        "work_dir": str(work_dir),
        "commands": [],
        "smoke_commands": [],
        "env": {},
        "tools": {},
    }


def _start_host_agent(prefix: str = "fleet-e2e-hmac-"):
    """Tempdirs + workspace_agent. Caller must stop proc and rmtree tmp."""
    tmp = Path(tempfile.mkdtemp(prefix=prefix))
    fleet = tmp / ".fleet"
    jobs = tmp / "jobs"
    bindir = tmp / "bin"
    src = tmp / "src-repo"
    fleet.mkdir()
    jobs.mkdir()
    bindir.mkdir()
    src.mkdir()
    (src / "README").write_text("hmac-src\n", encoding="utf-8")
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
    return tmp, fleet, jobs, src, proc, logf, log_path


def _stop_host_agent(tmp: Path, proc, logf) -> None:
    proc.send_signal(signal.SIGTERM)
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
    logf.close()
    shutil.rmtree(tmp, ignore_errors=True)


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
    """Signed provision + fleet-wsexec; HMAC fail-closed."""

    def test_agent_provision_and_wsexec(self):
        """Signed provision clones/commands; fleet-wsexec prints named-tool stdout."""
        self.assertTrue(AGENT.is_file(), f"missing workspace agent at {AGENT}")
        tmp, fleet, jobs, src, proc, logf, log_path = _start_host_agent(
            prefix="fleet-e2e-"
        )
        try:
            alive = fleet / "agent.alive"
            wsexec = tmp / "bin" / "fleet-wsexec"
            for _ in range(100):
                if alive.is_file() and wsexec.is_file():
                    break
                time.sleep(0.05)
            else:
                tail = log_path.read_text(encoding="utf-8")[-2000:]
                self.fail(f"agent.alive or fleet-wsexec missing\n{tail}")

            st = wsexec.stat()
            self.assertFalse(
                st.st_mode & 0o022,
                f"fleet-wsexec is group/other-writable: mode={oct(st.st_mode)}",
            )

            job_id = "e2e-job-1"
            work_dir = jobs / job_id / "repo"
            provision = _sign(
                {
                    "job_id": job_id,
                    "action": "provision",
                    "req_id": "prov-1",
                    "repo_url": str(src),
                    "git_ref": "main",
                    "work_dir": str(work_dir),
                    "commands": ["echo provisioned > marker.txt"],
                    "smoke_commands": [],
                    "env": {},
                    "tools": {
                        "hello": {"argv": ["echo", "e2e-hello"], "description": "echo"},
                    },
                }
            )
            result = _submit_request(fleet, provision, timeout=30)
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
                (work_dir / "marker.txt").is_file(),
                "provision command did not write marker.txt",
            )

            child_env = os.environ.copy()
            child_env["FLEET_DIR"] = str(fleet)
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
        finally:
            _stop_host_agent(tmp, proc, logf)

    def test_fleet_wsexec_usage_and_missing_FLEET_WORK_DIR_exit_2(self):
        """Installed wrapper exits 2 on 0 args, extra args, or no FLEET_WORK_DIR."""
        self.assertTrue(AGENT.is_file(), f"missing workspace agent at {AGENT}")
        tmp, fleet, jobs, src, proc, logf, log_path = _start_host_agent(
            prefix="fleet-e2e-wsexec-usage-"
        )
        try:
            alive = fleet / "agent.alive"
            wsexec = tmp / "bin" / "fleet-wsexec"
            for _ in range(100):
                if alive.is_file() and wsexec.is_file():
                    break
                time.sleep(0.05)
            else:
                tail = log_path.read_text(encoding="utf-8")[-2000:]
                self.fail(f"agent.alive or fleet-wsexec missing\n{tail}")

            child_env = os.environ.copy()
            child_env["FLEET_DIR"] = str(fleet)
            child_env["FLEET_JOB_ID"] = "usage-job"
            child_env["FLEET_WORK_DIR"] = str(jobs)
            child_env.pop("FLEET_HANDSHAKE_SECRET", None)

            no_args = subprocess.run(
                [str(wsexec)],
                env=child_env,
                capture_output=True,
                text=True,
                timeout=10,
            )
            self.assertEqual(no_args.returncode, 2, no_args.stderr)

            extra = subprocess.run(
                [str(wsexec), "hello", "extra"],
                env=child_env,
                capture_output=True,
                text=True,
                timeout=10,
            )
            self.assertEqual(extra.returncode, 2, extra.stderr)

            child_env.pop("FLEET_WORK_DIR", None)
            missing = subprocess.run(
                [str(wsexec), "hello"],
                env=child_env,
                capture_output=True,
                text=True,
                timeout=10,
            )
            self.assertEqual(missing.returncode, 2, missing.stderr)
        finally:
            _stop_host_agent(tmp, proc, logf)

    def test_signed_provision_hmac_replay_is_rejected(self):
        """SEEN_HMACS is one-shot: same signed provision must not clone again."""
        self.assertTrue(AGENT.is_file(), f"missing workspace agent at {AGENT}")
        tmp, fleet, jobs, src, proc, logf, log_path = _start_host_agent()
        try:
            alive = fleet / "agent.alive"
            for _ in range(100):
                if alive.is_file():
                    break
                time.sleep(0.05)
            else:
                tail = log_path.read_text(encoding="utf-8")[-2000:]
                self.fail(f"agent.alive missing\n{tail}")

            once_id = "hmac-replay-once"
            once_wd = jobs / once_id / "repo"
            once = _sign(_hmac_provision(once_id, once_id, str(src), once_wd))
            first = _submit_request(fleet, once, timeout=30)
            self.assertIsNotNone(first, "valid signed provision timed out")
            assert first is not None
            self.assertTrue(first.get("ok"), f"valid provision failed: {first}")
            self.assertTrue(once_wd.is_dir(), "valid provision must create work_dir")
            self.assertTrue(
                (once_wd / "README").is_file(),
                "valid provision must clone",
            )
            marker = once_wd / "no-second-clone"
            marker.write_text("keep\n", encoding="utf-8")

            (fleet / f"result-{once_id}.json").unlink()
            replay = _submit_request(fleet, once, timeout=30)
            self.assertIsNotNone(replay, "replayed signed request: no result")
            assert replay is not None
            self.assertFalse(replay.get("ok"), f"replay not rejected: {replay}")
            blob = f"{replay.get('error') or ''} {replay.get('stderr') or ''}".lower()
            self.assertIn("replayed", blob, replay)
            self.assertIn("hmac", blob, replay)
            self.assertTrue(
                marker.is_file(),
                "replay must not re-clone (git_clone rmtree dest)",
            )
        finally:
            _stop_host_agent(tmp, proc, logf)


if __name__ == "__main__":
    unittest.main(verbosity=2)
