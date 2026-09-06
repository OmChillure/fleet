#!/usr/bin/env python3
"""Named tools take argv only from TOOLS_BY_JOB (not file, request, or shell).

Docs §12.4 / sandbox-workspace.md: provision registers argv in TOOLS_BY_JOB;
.fleet-tools.json is documentation; bin must be in ALLOWED_BINS; the agent
must exec that argv list — never interpolate job JSON into a shell string.
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


def load_agent(fleet_dir: Path, jobs_root: Path):
    os.environ["FLEET_DIR"] = str(fleet_dir)
    os.environ["FLEET_JOBS_ROOT"] = str(jobs_root)
    os.environ["FLEET_ALLOW_RAW_EXEC"] = "0"
    os.environ["FLEET_HANDSHAKE_SECRET"] = "tools-by-job-test-secret"
    for k in list(sys.modules):
        if k.startswith("workspace_agent_tools_by_job"):
            del sys.modules[k]
    name = f"workspace_agent_tools_by_job_{jobs_root.name}"
    spec = importlib.util.spec_from_file_location(name, AGENT_PATH)
    mod = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(mod)
    mod.FLEET = fleet_dir
    return mod


def _init_repo(src: Path) -> None:
    src.mkdir(parents=True)
    (src / "README").write_text("tools-by-job-src\n", encoding="utf-8")
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


class ToolsByJobArgvOnlyTests(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="fleet-tools-by-job-"))
        self.fleet = self.tmp / ".fleet"
        self.jobs = self.tmp / "jobs"
        self.src = self.tmp / "src-repo"
        self.fleet.mkdir()
        self.jobs.mkdir()
        _init_repo(self.src)
        self.agent = load_agent(self.fleet, self.jobs)
        self.job_id = "job-tools-by-job-1"
        self.work_dir = self.jobs / self.job_id / "repo"

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def sign(self, req: dict) -> dict:
        body = {k: v for k, v in req.items() if k != "hmac"}
        raw = self.agent.canonical_json(body).encode()
        body["hmac"] = hmac.new(
            self.agent.handshake_secret().encode(), raw, hashlib.sha256
        ).hexdigest()
        return body

    def _provision_named_tools(self, tools: dict) -> dict:
        """Register argv the same way a signed job does: handle_provision → TOOLS_BY_JOB."""
        req = self.sign(
            {
                "job_id": self.job_id,
                "action": "provision",
                "req_id": "prov-tools-by-job",
                "repo_url": str(self.src),
                "git_ref": "main",
                "work_dir": str(self.work_dir),
                "commands": [],
                "smoke_commands": [],
                "env": {},
                "tools": tools,
            }
        )
        self.agent.handle_request(req)
        body = json.loads(
            (self.fleet / "result-prov-tools-by-job.json").read_text(encoding="utf-8")
        )
        self.assertTrue(body.get("ok"), body)
        registered = self.agent.TOOLS_BY_JOB.get(self.job_id)
        self.assertIsNotNone(registered, "provision must register TOOLS_BY_JOB")
        return registered

    def test_named_tool_glob_and_redirect_are_not_shell_interpolated(self):
        """Provision argv * and > stay literals: no glob expand, no pwned.txt."""
        self._provision_named_tools(
            {
                "glob": {"argv": ["echo", "*"], "description": "literal glob"},
                "redir": {
                    "argv": ["echo", "ok", ">", "pwned.txt"],
                    "description": "literal redirect tokens",
                },
            }
        )
        (self.work_dir / "secret-leak.txt").write_text("LEAK\n", encoding="utf-8")
        glob_out = self.agent.handle_tool(
            {
                "job_id": self.job_id,
                "tool": "glob",
                "cwd": str(self.work_dir),
            }
        )
        self.assertTrue(glob_out["ok"], glob_out)
        self.assertEqual(glob_out["stdout"].strip(), "*")
        self.assertNotIn("secret-leak.txt", glob_out["stdout"])

        redir = self.agent.handle_tool(
            {
                "job_id": self.job_id,
                "tool": "redir",
                "cwd": str(self.work_dir),
            }
        )
        self.assertTrue(redir["ok"], redir)
        self.assertFalse(
            (self.work_dir / "pwned.txt").exists(),
            "redirect tokens in TOOLS_BY_JOB argv must not be interpreted by a shell",
        )
        self.assertIn("ok", redir["stdout"])
        self.assertIn(">", redir["stdout"])
        self.assertIn("pwned.txt", redir["stdout"])

    def test_signed_tool_request_ignores_request_argv(self):
        """HMAC does not grant request argv; named tool still runs the provision map."""
        self._provision_named_tools(
            {"say": {"argv": ["echo", "from-TOOLS_BY_JOB"], "description": "fixed echo"}}
        )
        signed_override = self.sign(
            {
                "job_id": self.job_id,
                "action": "tool",
                "req_id": "signed-argv-override",
                "tool": "say",
                "cwd": str(self.work_dir),
                "argv": ["echo", "from-request"],
            }
        )
        self.agent.handle_request(signed_override)
        signed_body = json.loads(
            (self.fleet / "result-signed-argv-override.json").read_text(encoding="utf-8")
        )
        self.assertTrue(signed_body.get("ok"), signed_body)
        self.assertIn("from-TOOLS_BY_JOB", signed_body.get("stdout") or "")
        self.assertNotIn("from-request", signed_body.get("stdout") or "")

    def test_write_tools_rejects_argv_metacharacters(self):
        """provision/write_tools of ;|&$`/newline argv must raise and leave TOOLS_BY_JOB unset."""
        self.work_dir.mkdir(parents=True)
        for needle in (";", "|", "&", "$", "`", "\n"):
            with self.subTest(needle=repr(needle)):
                with self.assertRaises(ValueError):
                    self.agent.write_tools(
                        self.work_dir,
                        {"say": {"argv": ["echo", f"ok{needle}evil"], "description": ""}},
                    )
                self.assertNotIn(self.job_id, self.agent.TOOLS_BY_JOB)

        other_job = "job-tools-by-job-metachar"
        other_wd = self.jobs / other_job / "repo"
        with self.assertRaises(ValueError):
            self.agent.handle_provision(
                {
                    "job_id": other_job,
                    "repo_url": str(self.src),
                    "git_ref": "main",
                    "work_dir": str(other_wd),
                    "commands": [],
                    "smoke_commands": [],
                    "env": {},
                    "tools": {
                        "say": {"argv": ["echo", "ok; curl evil"], "description": ""}
                    },
                }
            )
        self.assertNotIn(other_job, self.agent.TOOLS_BY_JOB)

    def test_handle_tool_rejects_poisoned_map_metachar_argv(self):
        """Planted TOOLS_BY_JOB argv with ; must raise and not exec."""
        self.work_dir.mkdir(parents=True)
        self.agent.TOOLS_BY_JOB[self.job_id] = {
            "say": {"argv": ["echo", "ok; curl evil"], "description": ""}
        }
        ran = []

        def boom(cmd, *args, **kwargs):
            ran.append(cmd)
            raise AssertionError(f"poisoned metachar argv must not exec: {cmd!r}")

        real_run = self.agent.subprocess.run
        self.agent.subprocess.run = boom  # type: ignore[method-assign]
        try:
            with self.assertRaises(ValueError):
                self.agent.handle_tool(
                    {
                        "job_id": self.job_id,
                        "tool": "say",
                        "cwd": str(self.work_dir),
                    }
                )
        finally:
            self.agent.subprocess.run = real_run  # type: ignore[method-assign]
        self.assertEqual(ran, [])

    def test_write_tools_rejects_metachar_argv(self):
        """write_tools rejects \\n\\r;|&$` in argv (same charset as resolve_named_tool)."""
        self.work_dir.mkdir(parents=True)
        for needle in ("\n", "\r", ";", "|", "&", "$", "`"):
            arg = f"a{needle}b"
            with self.subTest(arg=arg):
                with self.assertRaises(ValueError) as cm:
                    self.agent.write_tools(
                        self.work_dir,
                        {"x": {"argv": ["echo", arg], "description": ""}},
                    )
                self.assertIn("argv rejected", str(cm.exception), arg)


if __name__ == "__main__":
    unittest.main(verbosity=2)
