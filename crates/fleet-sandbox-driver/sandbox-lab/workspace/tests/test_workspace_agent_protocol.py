#!/usr/bin/env python3
"""Headless integration tests against shipped workspace_agent.py protocol.

Exercises handle_tool for:
  (a) unknown tool rejected
  (b) known tool ok + stdout structure
  (c) cwd outside jobs root rejected

Does not re-implement policy — imports the real agent module.
"""
from __future__ import annotations

import importlib.util
import json
import os
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

AGENT_PATH = Path(__file__).resolve().parents[1] / "workspace_agent.py"


def load_agent(fleet_dir: Path, jobs_root: Path):
    """Load workspace_agent with isolated FLEET_DIR / FLEET_JOBS_ROOT."""
    os.environ["FLEET_DIR"] = str(fleet_dir)
    os.environ["FLEET_JOBS_ROOT"] = str(jobs_root)
    os.environ["FLEET_ALLOW_RAW_EXEC"] = "0"
    os.environ["FLEET_HANDSHAKE_SECRET"] = "protocol-test-secret"
    # Unique module name per load so env is re-read if needed
    name = f"workspace_agent_under_test_{jobs_root.name}"
    # Clear cached module if reloading
    for k in list(sys.modules):
        if k.startswith("workspace_agent_under_test"):
            del sys.modules[k]
    spec = importlib.util.spec_from_file_location(name, AGENT_PATH)
    mod = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    # Re-bind module-level FLEET after import by patching post-load
    spec.loader.exec_module(mod)
    mod.FLEET = fleet_dir
    return mod


class WorkspaceAgentProtocolTests(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="fleet-ws-agent-"))
        self.fleet = self.tmp / ".fleet"
        self.jobs = self.tmp / "jobs"
        self.fleet.mkdir(parents=True)
        self.jobs.mkdir(parents=True)
        self.job_id = "job-protocol-1"
        self.cwd = self.jobs / self.job_id / "repo"
        self.cwd.mkdir(parents=True)
        (self.cwd / "hello.txt").write_text("fleet-hello\n", encoding="utf-8")
        tools = {
            "cat_hello": {
                "argv": ["cat", "hello.txt"],
                "description": "read hello",
            },
            "echo_ok": {
                "argv": ["echo", "tool-ok"],
                "description": "echo",
            },
        }
        (self.cwd / ".fleet-tools.json").write_text(
            json.dumps(tools, indent=2) + "\n", encoding="utf-8"
        )
        self.agent = load_agent(self.fleet, self.jobs)
        # File is documentation only; argv comes from TOOLS_BY_JOB.
        self.agent.TOOLS_BY_JOB[self.job_id] = tools

    def sign(self, req: dict) -> dict:
        body = {k: v for k, v in req.items() if k != "hmac"}
        raw = self.agent.canonical_json(body).encode()
        secret = self.agent.handshake_secret().encode()
        import hmac as hm
        import hashlib
        body["hmac"] = hm.new(secret, raw, hashlib.sha256).hexdigest()
        return body

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def test_a_unknown_tool_rejected(self):
        with self.assertRaises(ValueError) as cm:
            self.agent.handle_tool(
                {
                    "job_id": self.job_id,
                    "action": "tool",
                    "tool": "rm_rf_not_listed",
                    "cwd": str(self.cwd),
                }
            )
        msg = str(cm.exception)
        self.assertIn("not allowlisted", msg)
        self.assertIn("rm_rf_not_listed", msg)

    def test_b_known_tool_ok_stdout_structure(self):
        result = self.agent.handle_tool(
            {
                "job_id": self.job_id,
                "action": "tool",
                "tool": "cat_hello",
                "cwd": str(self.cwd),
            }
        )
        self.assertTrue(result["ok"], result)
        self.assertEqual(result["tool"], "cat_hello")
        self.assertEqual(result["exit_code"], 0)
        self.assertEqual(result["argv"], ["cat", "hello.txt"])
        self.assertIn("fleet-hello", result["stdout"])
        self.assertEqual(result["job_id"], self.job_id)
        self.assertIn("stdout", result)
        self.assertIn("stderr", result)

    def test_b2_echo_tool(self):
        result = self.agent.handle_tool(
            {
                "job_id": self.job_id,
                "tool": "echo_ok",
                "cwd": str(self.cwd),
            }
        )
        self.assertTrue(result["ok"])
        self.assertIn("tool-ok", result["stdout"])

    def test_c_cwd_outside_jobs_rejected(self):
        outside = self.tmp / "evil"
        outside.mkdir()
        (outside / ".fleet-tools.json").write_text(
            json.dumps({"x": {"argv": ["echo", "no"]}}), encoding="utf-8"
        )
        with self.assertRaises(ValueError) as cm:
            self.agent.handle_tool(
                {
                    "job_id": self.job_id,
                    "tool": "x",
                    "cwd": str(outside),
                }
            )
        self.assertIn("outside job isolation", str(cm.exception))

    def test_c2_parent_escape_rejected(self):
        # path with .. should be rejected by path_under_jobs
        escape = self.cwd / ".." / ".." / "evil"
        self.assertFalse(self.agent.path_under_jobs(escape))

    def test_raw_exec_disabled(self):
        with self.assertRaises(ValueError) as cm:
            self.agent.handle_exec_legacy(
                {"job_id": self.job_id, "cmd": "echo pwned", "cwd": str(self.cwd)}
            )
        self.assertIn("raw exec disabled", str(cm.exception))

    def test_path_under_jobs_accepts_job_tree(self):
        self.assertTrue(self.agent.path_under_jobs(self.cwd))
        self.assertTrue(self.agent.path_under_jobs(self.jobs / "other" / "repo"))

    def test_handle_request_tool_writes_result(self):
        """Full request path: handle_request for action=tool writes result-{id}.json."""
        req = {
            "job_id": self.job_id,
            "action": "tool",
            "req_id": "t1",
            "tool": "echo_ok",
            "cwd": str(self.cwd),
        }
        self.agent.handle_request(self.sign(req))
        rpath = self.fleet / "result-t1.json"
        self.assertTrue(rpath.is_file(), rpath)
        body = json.loads(rpath.read_text(encoding="utf-8"))
        self.assertTrue(body["ok"])
        self.assertIn("tool-ok", body["stdout"])
        self.assertTrue(body.get("hmac"), "result must be HMAC-signed")
        unsigned = {k: v for k, v in body.items() if k != "hmac"}
        expect = self.agent.sign_body(unsigned)["hmac"]
        self.assertEqual(body["hmac"], expect)

    def test_handle_request_unknown_tool_fails_result(self):
        req = {
            "job_id": self.job_id,
            "action": "tool",
            "req_id": "t-bad",
            "tool": "nope",
            "cwd": str(self.cwd),
        }
        self.agent.handle_request(self.sign(req))
        body = json.loads((self.fleet / "result-t-bad.json").read_text(encoding="utf-8"))
        self.assertFalse(body["ok"])
        self.assertIn("not allowlisted", body.get("error") or body.get("stderr") or "")

    def test_unsigned_request_rejected_before_provision(self):
        other = "job-not-provisioned"
        req = {
            "job_id": other,
            "action": "tool",
            "req_id": "unsigned",
            "tool": "echo_ok",
            "cwd": str(self.cwd),
        }
        self.agent.handle_request(req)
        body = json.loads((self.fleet / "result-unsigned.json").read_text(encoding="utf-8"))
        self.assertFalse(body["ok"])
        err = (body.get("error") or "").lower()
        self.assertTrue(
            "hmac" in err or "before provision" in err or "no registered tools" in err,
            body,
        )

    def test_hmac_tool_does_not_read_tools_file(self):
        """Signed tool requests must not take argv from a planted tools file."""
        planted = "hmac-planted"
        (self.cwd / ".fleet-tools.json").write_text(
            json.dumps({"pwn": {"argv": ["echo", "from-file"]}}),
            encoding="utf-8",
        )
        req = self.sign(
            {
                "job_id": planted,
                "action": "tool",
                "req_id": "hmac-file",
                "tool": "pwn",
                "cwd": str(self.cwd),
            }
        )
        self.agent.handle_request(req)
        body = json.loads((self.fleet / "result-hmac-file.json").read_text(encoding="utf-8"))
        self.assertFalse(body["ok"], body)
        err = (body.get("error") or "").lower()
        self.assertIn("no registered tools", err)
        self.assertNotIn("from-file", (body.get("stdout") or ""))

    def test_unsigned_tool_after_register_uses_memory_argv(self):
        self.agent.TOOLS_BY_JOB[self.job_id] = {
            "echo_ok": {"argv": ["echo", "from-memory"], "description": ""}
        }
        (self.cwd / ".fleet-tools.json").write_text(
            json.dumps({"echo_ok": {"argv": ["echo", "from-file"]}}),
            encoding="utf-8",
        )
        req = {
            "job_id": self.job_id,
            "action": "tool",
            "req_id": "unsigned-ok",
            "tool": "echo_ok",
            "cwd": str(self.cwd),
        }
        self.agent.handle_request(req)
        body = json.loads((self.fleet / "result-unsigned-ok.json").read_text(encoding="utf-8"))
        self.assertTrue(body["ok"], body)
        self.assertIn("from-memory", body["stdout"])
        self.assertNotIn("from-file", body["stdout"])

    def test_unsigned_tool_rejects_argv_override(self):
        self.agent.TOOLS_BY_JOB[self.job_id] = {
            "echo_ok": {"argv": ["echo", "safe"], "description": ""}
        }
        req = {
            "job_id": self.job_id,
            "action": "tool",
            "req_id": "unsigned-argv",
            "tool": "echo_ok",
            "cwd": str(self.cwd),
            "argv": ["curl", "http://evil.example"],
        }
        self.agent.handle_request(req)
        body = json.loads((self.fleet / "result-unsigned-argv.json").read_text(encoding="utf-8"))
        self.assertFalse(body["ok"], body)
        self.assertIn("argv", (body.get("error") or "").lower())

    def test_git_ref_rejects_shell_metachar(self):
        with self.assertRaises(ValueError):
            self.agent.validate_git_arg("git_ref", 'main"; curl evil')
        with self.assertRaises(ValueError):
            self.agent.validate_git_arg("git_ref", "-evil")

    def test_git_clone_missing_ref_fails_closed(self):
        import subprocess

        src = self.tmp / "src-repo"
        src.mkdir()
        (src / "README").write_text("only-main\n", encoding="utf-8")
        subprocess.run(["git", "init", "-b", "main", str(src)], check=True, capture_output=True)
        subprocess.run(
            ["git", "-C", str(src), "config", "user.email", "t@example.test"],
            check=True,
        )
        subprocess.run(["git", "-C", str(src), "config", "user.name", "t"], check=True)
        subprocess.run(["git", "-C", str(src), "add", "README"], check=True)
        subprocess.run(["git", "-C", str(src), "commit", "-q", "-m", "init"], check=True)
        dest = self.tmp / "clone-dest"
        with self.assertRaises(RuntimeError) as cm:
            self.agent.git_clone(str(src), "no-such-ref", dest, os.environ.copy())
        self.assertIn("checkout", str(cm.exception).lower())
        self.assertIn("no-such-ref", str(cm.exception))

    def test_mutated_tools_file_ignored_after_register(self):
        self.agent.TOOLS_BY_JOB[self.job_id] = {
            "echo_ok": {"argv": ["echo", "from-memory"], "description": ""}
        }
        (self.cwd / ".fleet-tools.json").write_text(
            json.dumps({"echo_ok": {"argv": ["echo", "from-file"]}}),
            encoding="utf-8",
        )
        result = self.agent.handle_tool(
            {"job_id": self.job_id, "tool": "echo_ok", "cwd": str(self.cwd)}
        )
        self.assertIn("from-memory", result["stdout"])
        self.assertNotIn("from-file", result["stdout"])

    def test_curl_and_bash_argv_rejected(self):
        with self.assertRaises(ValueError):
            self.agent.validate_tool_bin("curl")
        with self.assertRaises(ValueError):
            self.agent.validate_tool_bin("bash")

    def test_ssh_repo_detection(self):
        self.assertTrue(self.agent.is_ssh_repo("git@github.com:org/repo.git"))
        self.assertTrue(
            self.agent.is_ssh_repo("ssh://git@host.docker.internal:2222/srv/git/hello.git")
        )
        self.assertFalse(self.agent.is_ssh_repo("https://github.com/org/repo.git"))

    def test_write_json_default_mode_is_group_readable(self):
        path = self.fleet / "result-mode.json"
        self.agent.write_json(path, {"ok": True})
        mode = path.stat().st_mode & 0o777
        self.assertEqual(mode, 0o664, oct(mode))

    def test_git_ssh_env_sets_command_when_key_present(self):
        key = self.tmp / "id_ed25519"
        key.write_text("dummy-key\n", encoding="utf-8")
        os.environ["GIT_SSH_KEY_PATH"] = str(key)
        os.environ["HOME"] = str(self.tmp / "home")
        (self.tmp / "home").mkdir()
        try:
            env = self.agent.git_ssh_env({})
            self.assertIn("GIT_SSH_COMMAND", env)
            self.assertIn("ssh -F", env["GIT_SSH_COMMAND"])
            ident = self.tmp / "home" / ".ssh" / "id_fleet"
            self.assertTrue(ident.is_file())
        finally:
            os.environ.pop("GIT_SSH_KEY_PATH", None)


if __name__ == "__main__":
    unittest.main(verbosity=2)
