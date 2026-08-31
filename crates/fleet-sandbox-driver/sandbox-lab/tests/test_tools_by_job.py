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

    def test_TOOLS_BY_JOB_named_tools_take_argv_only_not_shell_interpolated(self):
        """Argv is the provision map only: not the tools file, not the request, not bash -lc."""
        provision_tools = {
            "say": {
                "argv": ["echo", "from-TOOLS_BY_JOB"],
                "description": "fixed echo",
            },
            "glob": {"argv": ["echo", "*"], "description": "literal glob"},
            "redir": {
                "argv": ["echo", "ok", ">", "pwned.txt"],
                "description": "literal redirect tokens",
            },
        }
        registered = self._provision_named_tools(provision_tools)
        self.assertEqual(registered["say"]["argv"], ["echo", "from-TOOLS_BY_JOB"])
        self.assertEqual(registered["glob"]["argv"], ["echo", "*"])
        self.assertEqual(registered["redir"]["argv"], ["echo", "ok", ">", "pwned.txt"])

        (self.work_dir / "secret-leak.txt").write_text("LEAK\n", encoding="utf-8")
        planted = {
            "say": {"argv": ["echo", "from-file"]},
            "glob": {"argv": ["echo", "from-file-glob"]},
            "pwn": {"argv": ["echo", "from-file-pwn"]},
        }
        tools_path = self.work_dir / ".fleet-tools.json"
        try:
            tools_path.write_text(json.dumps(planted), encoding="utf-8")
        except PermissionError:
            tools_path.unlink()
            tools_path.write_text(json.dumps(planted), encoding="utf-8")

        real_run = self.agent.subprocess.run
        seen_cmds: list = []

        def spy(cmd, *args, **kwargs):
            seen_cmds.append(list(cmd) if isinstance(cmd, (list, tuple)) else cmd)
            self.assertFalse(
                kwargs.get("shell"),
                "named tool exec must not set shell=True (job JSON must not become a shell string)",
            )
            self.assertIsInstance(
                cmd, (list, tuple), "tool exec must be an argv list, not a shell string"
            )
            argv0 = str(cmd[0]) if cmd else ""
            argv1 = str(cmd[1]) if len(cmd) > 1 else ""
            # Named tools never go through bash/sh -lc. That is how job JSON
            # would be interpolated (run_cmd). Argv list only.
            self.assertNotIn(
                Path(argv0).name,
                {"bash", "sh"},
                f"named tool must not exec a shell: {cmd!r}",
            )
            self.assertNotIn(argv1, ("-lc", "-c", "-l"))
            joined = " ".join(str(x) for x in cmd)
            self.assertNotIn("from-request", joined)
            self.assertNotIn("from-file", joined)
            self.assertNotIn('"argv"', joined)
            return real_run(cmd, *args, **kwargs)

        self.agent.subprocess.run = spy  # type: ignore[method-assign]
        try:
            result = self.agent.handle_tool(
                {
                    "job_id": self.job_id,
                    "action": "tool",
                    "tool": "say",
                    "cwd": str(self.work_dir),
                    "argv": ["echo", "from-request"],
                    "cmd": "echo from-request-cmd",
                }
            )
            self.assertTrue(result["ok"], result)
            self.assertEqual(result["argv"], ["echo", "from-TOOLS_BY_JOB"])
            self.assertIn("from-TOOLS_BY_JOB", result["stdout"])
            self.assertNotIn("from-file", result["stdout"])
            self.assertNotIn("from-request", result["stdout"])
            self.assertIn(["echo", "from-TOOLS_BY_JOB"], seen_cmds)

            glob_out = self.agent.handle_tool(
                {
                    "job_id": self.job_id,
                    "tool": "glob",
                    "cwd": str(self.work_dir),
                }
            )
            self.assertTrue(glob_out["ok"], glob_out)
            self.assertEqual(glob_out["argv"], ["echo", "*"])
            self.assertEqual(glob_out["stdout"].strip(), "*")
            self.assertNotIn("secret-leak.txt", glob_out["stdout"])
            self.assertNotIn("from-file", glob_out["stdout"])
            self.assertIn(["echo", "*"], seen_cmds)

            redir = self.agent.handle_tool(
                {
                    "job_id": self.job_id,
                    "tool": "redir",
                    "cwd": str(self.work_dir),
                }
            )
            self.assertTrue(redir["ok"], redir)
            self.assertEqual(redir["argv"], ["echo", "ok", ">", "pwned.txt"])
            self.assertFalse(
                (self.work_dir / "pwned.txt").exists(),
                "redirect tokens in TOOLS_BY_JOB argv must not be interpreted by a shell",
            )
            self.assertIn("ok", redir["stdout"])
            self.assertIn(">", redir["stdout"])
            self.assertIn("pwned.txt", redir["stdout"])
            self.assertIn(["echo", "ok", ">", "pwned.txt"], seen_cmds)

            # Name-only unsigned request is the fleet-wsexec harness path.
            name_only = {
                "job_id": self.job_id,
                "action": "tool",
                "req_id": "unsigned-name-only",
                "tool": "say",
                "cwd": str(self.work_dir),
            }
            self.agent.handle_request(name_only)
            name_body = json.loads(
                (self.fleet / "result-unsigned-name-only.json").read_text(encoding="utf-8")
            )
            self.assertTrue(name_body.get("ok"), name_body)
            self.assertEqual(name_body.get("argv"), ["echo", "from-TOOLS_BY_JOB"])
            self.assertIn("from-TOOLS_BY_JOB", name_body.get("stdout") or "")
            self.assertNotIn("from-file", name_body.get("stdout") or "")

            unsigned = {
                "job_id": self.job_id,
                "action": "tool",
                "req_id": "unsigned-argv-override",
                "tool": "say",
                "cwd": str(self.work_dir),
                "argv": ["echo", "from-request"],
            }
            self.agent.handle_request(unsigned)
            unsigned_body = json.loads(
                (self.fleet / "result-unsigned-argv-override.json").read_text(encoding="utf-8")
            )
            self.assertFalse(unsigned_body.get("ok"), unsigned_body)
            self.assertIn("argv", (unsigned_body.get("error") or "").lower())
            self.assertNotIn("from-request", unsigned_body.get("stdout") or "")

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
            # HMAC does not grant request argv. Named tool still runs TOOLS_BY_JOB.
            self.assertTrue(signed_body.get("ok"), signed_body)
            self.assertEqual(signed_body.get("argv"), ["echo", "from-TOOLS_BY_JOB"])
            self.assertIn("from-TOOLS_BY_JOB", signed_body.get("stdout") or "")
            self.assertNotIn("from-request", signed_body.get("stdout") or "")
            self.assertNotIn("from-file", signed_body.get("stdout") or "")
        finally:
            self.agent.subprocess.run = real_run  # type: ignore[method-assign]

        with self.assertRaises(ValueError) as cm:
            self.agent.handle_tool(
                {
                    "job_id": "job-never-provisioned",
                    "tool": "say",
                    "cwd": str(self.work_dir),
                }
            )
        self.assertIn("no registered tools", str(cm.exception))
        self.assertIn(".fleet-tools.json", str(cm.exception))

        with self.assertRaises(ValueError) as cm:
            self.agent.handle_tool(
                {
                    "job_id": self.job_id,
                    "tool": "pwn",
                    "cwd": str(self.work_dir),
                }
            )
        self.assertIn("not allowlisted", str(cm.exception))

    def test_TOOLS_BY_JOB_bin_must_be_in_ALLOWED_BINS(self):
        """curl / bash / path-to-curl never register or exec; echo stays allowlisted."""
        self.work_dir.mkdir(parents=True)
        for argv0 in ("curl", "bash", "sh", "/usr/bin/curl", "/bin/bash"):
            with self.subTest(argv0=argv0):
                with self.assertRaises(ValueError) as cm:
                    self.agent.write_tools(
                        self.work_dir,
                        {"x": {"argv": [argv0, "http://evil.example"], "description": ""}},
                    )
                msg = str(cm.exception).lower()
                self.assertIn("allowlisted", msg)
                self.assertIn(Path(argv0).name.lower(), msg)

        written = self.agent.write_tools(
            self.work_dir,
            {"say": {"argv": ["echo", "allowed-bin"], "description": ""}},
        )
        self.assertEqual(written["say"]["argv"], ["echo", "allowed-bin"])
        self.assertIn("echo", self.agent.ALLOWED_BINS)
        self.assertNotIn("curl", self.agent.ALLOWED_BINS)
        self.assertNotIn("bash", self.agent.ALLOWED_BINS)
        self.assertNotIn("sh", self.agent.ALLOWED_BINS)

        # Even a poisoned in-memory map must not exec a non-allowlisted bin.
        self.agent.TOOLS_BY_JOB[self.job_id] = {
            "evil": {"argv": ["curl", "http://evil.example"], "description": ""}
        }
        with self.assertRaises(ValueError) as cm:
            self.agent.handle_tool(
                {
                    "job_id": self.job_id,
                    "tool": "evil",
                    "cwd": str(self.work_dir),
                }
            )
        self.assertIn("allowlisted", str(cm.exception).lower())

        self.agent.TOOLS_BY_JOB[self.job_id] = {
            "ok": {"argv": ["echo", "allowed-bin"], "description": ""}
        }
        result = self.agent.handle_tool(
            {
                "job_id": self.job_id,
                "tool": "ok",
                "cwd": str(self.work_dir),
            }
        )
        self.assertTrue(result["ok"], result)
        self.assertIn("allowed-bin", result["stdout"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
