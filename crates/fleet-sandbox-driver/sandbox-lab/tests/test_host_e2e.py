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

    def _assert_hmac_rejected(self, result: dict | None, label: str) -> None:
        self.assertIsNotNone(result, f"{label}: no result")
        assert result is not None
        self.assertFalse(result.get("ok"), f"{label} not rejected: {result}")
        self.assertNotIn(
            "clone_done",
            json.dumps(result).lower(),
            f"{label} must not provision: {result}",
        )

    def test_hmac_rejects_wrong_key_missing_sig_truncated_mac_and_replay(self):
        """Wrong key, missing sig, truncated mac, and replay fail closed."""
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

            missing_id = "hmac-missing"
            missing_wd = jobs / missing_id / "repo"
            missing = _submit_request(
                fleet,
                _hmac_provision(missing_id, missing_id, str(src), missing_wd),
            )
            self._assert_hmac_rejected(missing, "missing hmac")
            self.assertFalse(
                missing_wd.exists(),
                "missing hmac must not create work_dir",
            )

            wrong_id = "hmac-wrong-key"
            wrong_wd = jobs / wrong_id / "repo"
            wrong = _submit_request(
                fleet,
                _sign_with(
                    _hmac_provision(wrong_id, wrong_id, str(src), wrong_wd),
                    "wrong-handshake-secret",
                ),
            )
            self._assert_hmac_rejected(wrong, "wrong-key hmac")
            self.assertFalse(
                wrong_wd.exists(),
                "wrong-key hmac must not create work_dir",
            )

            trunc_id = "hmac-truncated"
            trunc_wd = jobs / trunc_id / "repo"
            signed_trunc = _sign(
                _hmac_provision(trunc_id, trunc_id, str(src), trunc_wd)
            )
            mac = signed_trunc["hmac"]
            self.assertGreater(len(mac), 16)
            signed_trunc["hmac"] = mac[:16]
            truncated = _submit_request(fleet, signed_trunc)
            self._assert_hmac_rejected(truncated, "truncated hmac")
            self.assertFalse(
                trunc_wd.exists(),
                "truncated hmac must not create work_dir",
            )

            captured = _sign(
                _hmac_provision(
                    "hmac-replay-src",
                    "hmac-replay-src",
                    str(src),
                    jobs / "hmac-replay-src" / "repo",
                )
            )
            replay_mut_id = "hmac-replay-mac"
            replay_mut_wd = jobs / replay_mut_id / "repo"
            transplanted = dict(captured)
            transplanted["req_id"] = replay_mut_id
            transplanted["job_id"] = replay_mut_id
            transplanted["work_dir"] = str(replay_mut_wd)
            replay_mut = _submit_request(fleet, transplanted)
            self._assert_hmac_rejected(replay_mut, "replayed hmac on mutated body")
            self.assertFalse(
                replay_mut_wd.exists(),
                "replayed hmac must not create work_dir",
            )

            once_id = "hmac-replay-once"
            once_wd = jobs / once_id / "repo"
            once = _sign(_hmac_provision(once_id, once_id, str(src), once_wd))
            first = _submit_request(fleet, once, timeout=30)
            self.assertIsNotNone(first, "valid signed provision timed out")
            assert first is not None
            self.assertTrue(first.get("ok"), f"valid provision failed: {first}")
            self.assertTrue(once_wd.is_dir(), "valid provision must create work_dir")
            (fleet / f"result-{once_id}.json").unlink()
            replay_same = _submit_request(fleet, once, timeout=30)
            self._assert_hmac_rejected(replay_same, "replayed signed request")
        finally:
            _stop_host_agent(tmp, proc, logf)


if __name__ == "__main__":
    unittest.main(verbosity=2)
