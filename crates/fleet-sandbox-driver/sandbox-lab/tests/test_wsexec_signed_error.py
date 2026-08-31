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

UNKNOWN_BINS = ("curl", "bash", "sh", "/usr/bin/curl", "/bin/bash", "./curl")
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

    def test_wsexec_unknown_bin_path_escape_bad_hmac_is_signed_error_not_a_shell(self):
        """Unknown bin, path escape, and invalid HMAC are signed errors; nothing execs."""
        self.assertIn("echo", self.agent.ALLOWED_BINS)
        for banned in ("curl", "bash", "sh"):
            self.assertNotIn(banned, self.agent.ALLOWED_BINS)

        self._provision_ok()
        self.seen_cmds.clear()

        # --- unknown bin: fleet-wsexec name-only against a poisoned map ---
        for argv0 in UNKNOWN_BINS:
            with self.subTest(case="unknown-bin-wsexec", argv0=argv0):
                self.seen_cmds.clear()
                self.agent.TOOLS_BY_JOB[self.job_id] = {
                    "evil": {"argv": [argv0, "http://evil.example", PWN_MARK], "description": ""}
                }
                req_id = f"wsexec-unknown-bin-{Path(argv0).name}-{argv0.count('/')}"
                self.agent.handle_request(
                    {
                        "job_id": self.job_id,
                        "action": "tool",
                        "req_id": req_id,
                        "tool": "evil",
                        "cwd": str(self.work_dir),
                    }
                )
                body = self._read_result(req_id)
                self._assert_signed_error(body, "allowlisted")
                self.assertIn(Path(argv0).name.lower(), (body.get("error") or "").lower())
                self._assert_never_ran(argv0, "curl", "bash", "sh")
                self._assert_never_ran_shell()
                self._assert_never_ran_pwn()

        # --- unknown bin: signed provision must not register or exec it ---
        for i, argv0 in enumerate(("curl", "bash", "/usr/bin/curl")):
            with self.subTest(case="unknown-bin-provision", argv0=argv0):
                self.seen_cmds.clear()
                job = f"job-prov-bin-{i}-{Path(argv0).name}"
                wd = self.jobs / job / "repo"
                req_id = f"prov-unknown-bin-{i}-{Path(argv0).name}"
                self.agent.handle_request(
                    self.sign(
                        {
                            "job_id": job,
                            "action": "provision",
                            "req_id": req_id,
                            "repo_url": str(self.src),
                            "git_ref": "main",
                            "work_dir": str(wd),
                            "commands": [],
                            "smoke_commands": [],
                            "env": {},
                            "tools": {
                                "evil": {
                                    "argv": [argv0, "http://evil.example", PWN_MARK],
                                    "description": "",
                                }
                            },
                        }
                    )
                )
                body = self._read_result(req_id)
                self._assert_signed_error(body, "allowlisted")
                self.assertIn(Path(argv0).name.lower(), (body.get("error") or "").lower())
                self._assert_never_ran(argv0)
                self.assertIsNone(
                    self.agent.TOOLS_BY_JOB.get(job),
                    "unknown bin must not land in TOOLS_BY_JOB",
                )
                joined = " ".join(" ".join(str(x) for x in cmd) for cmd in self.seen_cmds)
                self.assertNotIn(PWN_MARK, joined, f"unknown bin reached exec: {self.seen_cmds!r}")

        # restore a valid map for later cases
        self.agent.TOOLS_BY_JOB[self.job_id] = {
            "say": {"argv": ["echo", "wsexec-ok"], "description": ""}
        }
        self.seen_cmds.clear()

        # --- path escape: cwd outside jobs root (wsexec cwd) ---
        outside = self.tmp / "outside-jobs"
        outside.mkdir()
        (outside / f"{PWN_MARK}.txt").write_text("planted\n", encoding="utf-8")
        self.agent.handle_request(
            {
                "job_id": self.job_id,
                "action": "tool",
                "req_id": "wsexec-cwd-outside",
                "tool": "say",
                "cwd": str(outside),
            }
        )
        body = self._read_result("wsexec-cwd-outside")
        self._assert_signed_error(body, "outside")
        self._assert_never_ran_shell()
        echo_after_escape = [c for c in self.seen_cmds if c and Path(str(c[0])).name == "echo"]
        self.assertEqual(echo_after_escape, [], f"escaped cwd must not exec: {self.seen_cmds!r}")

        # --- path escape: logical .. walk out of jobs to a real directory ---
        self.seen_cmds.clear()
        escape = self.work_dir / ".." / ".." / ".." / "outside-jobs"
        self.assertTrue(escape.resolve() == outside.resolve(), escape)
        self.assertTrue(escape.is_dir(), escape)
        self.assertFalse(self.agent.path_under_jobs(escape))
        self.agent.handle_request(
            {
                "job_id": self.job_id,
                "action": "tool",
                "req_id": "wsexec-cwd-dotdot",
                "tool": "say",
                "cwd": str(escape),
            }
        )
        body = self._read_result("wsexec-cwd-dotdot")
        self._assert_signed_error(body, "outside")
        echo_after_dotdot = [c for c in self.seen_cmds if c and Path(str(c[0])).name == "echo"]
        self.assertEqual(echo_after_dotdot, [], f".. cwd must not exec: {self.seen_cmds!r}")

        # --- path escape: symlink whose resolve() leaves jobs ---
        self.seen_cmds.clear()
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

        # --- path escape: provision work_dir outside jobs ---
        self.seen_cmds.clear()
        escaped_wd = self.tmp / "escaped-work"
        self.agent.handle_request(
            self.sign(
                {
                    "job_id": "job-escape-wd",
                    "action": "provision",
                    "req_id": "prov-wd-escape",
                    "repo_url": str(self.src),
                    "git_ref": "main",
                    "work_dir": str(escaped_wd),
                    "commands": [f"echo {PWN_MARK}"],
                    "smoke_commands": [],
                    "env": {},
                    "tools": {},
                }
            )
        )
        body = self._read_result("prov-wd-escape")
        self._assert_signed_error(body, "outside")
        self.assertFalse(escaped_wd.exists(), "escaped work_dir must not be created")
        git_clones = [
            c
            for c in self.seen_cmds
            if c and Path(str(c[0])).name == "git" and "clone" in c
        ]
        self.assertEqual(git_clones, [], f"escaped work_dir must not clone: {self.seen_cmds!r}")
        self._assert_never_ran_shell()

        # --- bad HMAC: wrong key / truncated / missing must not exec ---
        hmac_cases = []
        missing = {
            "job_id": "job-hmac-missing",
            "action": "provision",
            "req_id": "hmac-missing",
            "repo_url": str(self.src),
            "git_ref": "main",
            "work_dir": str(self.jobs / "job-hmac-missing" / "repo"),
            "commands": [f"echo {PWN_MARK}"],
            "smoke_commands": [],
            "env": {},
            "tools": {},
        }
        hmac_cases.append(("missing", missing, self.jobs / "job-hmac-missing" / "repo"))

        wrong = self.sign(
            {
                "job_id": "job-hmac-wrong",
                "action": "provision",
                "req_id": "hmac-wrong",
                "repo_url": str(self.src),
                "git_ref": "main",
                "work_dir": str(self.jobs / "job-hmac-wrong" / "repo"),
                "commands": [f"echo {PWN_MARK}"],
                "smoke_commands": [],
                "env": {},
                "tools": {},
            },
            secret="wrong-handshake-secret",
        )
        hmac_cases.append(("wrong-key", wrong, self.jobs / "job-hmac-wrong" / "repo"))

        trunc = self.sign(
            {
                "job_id": "job-hmac-trunc",
                "action": "provision",
                "req_id": "hmac-trunc",
                "repo_url": str(self.src),
                "git_ref": "main",
                "work_dir": str(self.jobs / "job-hmac-trunc" / "repo"),
                "commands": [f"echo {PWN_MARK}"],
                "smoke_commands": [],
                "env": {},
                "tools": {},
            }
        )
        trunc["hmac"] = trunc["hmac"][:16]
        hmac_cases.append(("truncated", trunc, self.jobs / "job-hmac-trunc" / "repo"))

        for label, req, wd in hmac_cases:
            with self.subTest(case="bad-hmac-provision", label=label):
                self.seen_cmds.clear()
                self.agent.handle_request(req)
                body = self._read_result(req["req_id"])
                self._assert_signed_error(body, "hmac")
                self.assertFalse(wd.exists(), f"{label} hmac must not create work_dir")
                git_clones = [
                    c
                    for c in self.seen_cmds
                    if c and Path(str(c[0])).name == "git"
                ]
                self.assertEqual(
                    git_clones,
                    [],
                    f"{label} hmac must not exec git: {self.seen_cmds!r}",
                )
                self._assert_never_ran_shell()
                self._assert_never_ran("curl", "bash", "sh")

        # bad HMAC on a tool request that would otherwise exec
        self.seen_cmds.clear()
        bad_tool = self.sign(
            {
                "job_id": self.job_id,
                "action": "tool",
                "req_id": "hmac-bad-tool",
                "tool": "say",
                "cwd": str(self.work_dir),
            },
            secret="wrong-handshake-secret",
        )
        self.agent.handle_request(bad_tool)
        body = self._read_result("hmac-bad-tool")
        self._assert_signed_error(body, "hmac")
        echo_after_bad_mac = [c for c in self.seen_cmds if c and Path(str(c[0])).name == "echo"]
        self.assertEqual(
            echo_after_bad_mac,
            [],
            f"invalid HMAC must not exec the tool: {self.seen_cmds!r}",
        )
        self._assert_never_ran_shell()

        # --- control: name-only wsexec of an allowlisted bin still runs ---
        self.seen_cmds.clear()
        self.agent.handle_request(
            {
                "job_id": self.job_id,
                "action": "tool",
                "req_id": "wsexec-control-ok",
                "tool": "say",
                "cwd": str(self.work_dir),
            }
        )
        ok = self._read_result("wsexec-control-ok")
        self.assertTrue(ok.get("ok"), ok)
        self._assert_signed(ok)
        self.assertEqual(ok.get("argv"), ["echo", "wsexec-ok"])
        self.assertIn("wsexec-ok", ok.get("stdout") or "")
        self.assertIn(["echo", "wsexec-ok"], self.seen_cmds)


if __name__ == "__main__":
    unittest.main(verbosity=2)
