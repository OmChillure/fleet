#!/usr/bin/env python3
"""Headless integration tests against shipped workspace_agent.py protocol.

Exercises handle_tool for:
  (a) unknown tool rejected
  (b) known tool ok + stdout structure
  (c) cwd outside jobs root rejected

Does not re-implement policy — imports the real agent module.
"""
from __future__ import annotations

import hashlib
import hmac
import importlib.util
import json
import os
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

AGENT_PATH = Path(__file__).resolve().parents[1] / "workspace_agent.py"


def load_agent(
    fleet_dir: Path,
    jobs_root: Path,
    allow_raw: str | None = "0",
    handshake_secret: str | None = "protocol-test-secret",
):
    """Load workspace_agent with isolated FLEET_DIR / FLEET_JOBS_ROOT."""
    os.environ["FLEET_DIR"] = str(fleet_dir)
    os.environ["FLEET_JOBS_ROOT"] = str(jobs_root)
    if allow_raw is None:
        os.environ.pop("FLEET_ALLOW_RAW_EXEC", None)
    else:
        os.environ["FLEET_ALLOW_RAW_EXEC"] = allow_raw
    if handshake_secret is None:
        os.environ.pop("FLEET_HANDSHAKE_SECRET", None)
    else:
        os.environ["FLEET_HANDSHAKE_SECRET"] = handshake_secret
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
        self.assertIn("fleet-hello", result["stdout"])

    def test_handle_tool_nonzero_exit_ok_false(self):
        self.agent.TOOLS_BY_JOB[self.job_id]["cat_missing"] = {
            "argv": ["cat", "missing-file"],
            "description": "nonzero",
        }
        result = self.agent.handle_tool(
            {
                "job_id": self.job_id,
                "tool": "cat_missing",
                "cwd": str(self.cwd),
            }
        )
        self.assertFalse(result["ok"], result)
        self.assertNotEqual(result["exit_code"], 0)
        self.assertTrue(result["stderr"])

    def test_handle_tool_empty_argv_rejected(self):
        self.agent.TOOLS_BY_JOB[self.job_id]["empty"] = {
            "argv": [],
            "description": "invalid",
        }
        with self.assertRaises(ValueError) as cm:
            self.agent.handle_tool(
                {
                    "job_id": self.job_id,
                    "tool": "empty",
                    "cwd": str(self.cwd),
                }
            )
        self.assertIn("invalid argv", str(cm.exception))

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

    def test_raw_exec_policy_matches_rust(self):
        """Truth table must match crates/fleet-sandbox-driver/src/policy.rs raw_exec_allowed."""
        for off in (None, "", "   ", "0", "false", "no", "off", "FALSE"):
            self.assertFalse(self.agent.raw_exec_allowed(off), off)
        for on in ("1", "true", "yes", "on", "TRUE", " Yes ", "ON", " 1 "):
            self.assertTrue(self.agent.raw_exec_allowed(on), on)

    def test_raw_exec_enabled_when_allow_flag(self):
        self.agent = load_agent(self.fleet, self.jobs, allow_raw="1")
        req = self.sign(
            {
                "job_id": self.job_id,
                "action": "exec",
                "req_id": "raw-on",
                "cmd": "echo pwned-allowed",
                "cwd": str(self.cwd),
            }
        )
        self.agent.handle_request(req)
        body = json.loads((self.fleet / "result-raw-on.json").read_text(encoding="utf-8"))
        self.assertTrue(body["ok"], body)
        self.assertIn("pwned-allowed", body["stdout"])
        self.assertEqual(body["cmd"], "echo pwned-allowed")

    def test_handle_request_exec_rejected_by_default(self):
        req = self.sign(
            {
                "job_id": self.job_id,
                "action": "exec",
                "req_id": "raw-off",
                "cmd": "echo should-not-run",
                "cwd": str(self.cwd),
            }
        )
        self.agent.handle_request(req)
        body = json.loads((self.fleet / "result-raw-off.json").read_text(encoding="utf-8"))
        self.assertFalse(body["ok"], body)
        self.assertIn("raw exec disabled", body.get("error") or "")
        self.assertNotIn("should-not-run", body.get("stdout") or "")

    def test_handle_request_unknown_tool_writes_signed_failed_result(self):
        req = self.sign(
            {
                "job_id": self.job_id,
                "action": "tool",
                "req_id": "t-bad",
                "tool": "nope",
                "cwd": str(self.cwd),
            }
        )
        self.agent.handle_request(req)
        body = json.loads((self.fleet / "result-t-bad.json").read_text(encoding="utf-8"))
        self.assertFalse(body["ok"], body)
        self.assertEqual(body.get("stdout"), "")
        self.assertIn("nope", body.get("error") or "")
        unsigned = {k: v for k, v in body.items() if k != "hmac"}
        expect = self.agent.sign_body(unsigned)["hmac"]
        self.assertTrue(
            hmac.compare_digest(str(body.get("hmac") or ""), str(expect)),
            body,
        )

    def test_handle_request_empty_tool_name_fails_result(self):
        req = self.sign(
            {
                "job_id": self.job_id,
                "action": "tool",
                "req_id": "t-empty",
                "tool": "",
                "cwd": str(self.cwd),
            }
        )
        self.agent.handle_request(req)
        body = json.loads((self.fleet / "result-t-empty.json").read_text(encoding="utf-8"))
        self.assertFalse(body["ok"], body)
        self.assertEqual(body.get("stdout"), "")

    def test_unsigned_request_rejected_before_provision(self):
        other = "job-not-provisioned"
        self.assertNotIn(other, self.agent.TOOLS_BY_JOB)
        req = {
            "job_id": other,
            "action": "tool",
            "req_id": "unsigned",
            "tool": "echo_ok",
            "cwd": str(self.cwd),
        }
        self.agent.handle_request(req)
        body = json.loads((self.fleet / "result-unsigned.json").read_text(encoding="utf-8"))
        self.assertFalse(body["ok"], body)
        err = (body.get("error") or "").lower()
        self.assertIn("unsigned tool request before provision", err, body)
        stdout = body.get("stdout") or ""
        self.assertEqual(stdout, "", body)
        self.assertNotIn("tool-ok", stdout)

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
            "argv": ["echo", "from-request"],
        }
        self.agent.handle_request(req)
        body = json.loads((self.fleet / "result-unsigned-argv.json").read_text(encoding="utf-8"))
        self.assertFalse(body["ok"], body)
        stdout = body.get("stdout") or ""
        self.assertEqual(stdout, "", body)
        self.assertNotIn("from-request", stdout)

    def test_unsigned_tool_rejects_cmd_override(self):
        self.agent.TOOLS_BY_JOB[self.job_id] = {
            "echo_ok": {"argv": ["echo", "safe"], "description": ""}
        }
        req = {
            "job_id": self.job_id,
            "action": "tool",
            "req_id": "unsigned-cmd",
            "tool": "echo_ok",
            "cwd": str(self.cwd),
            "cmd": "echo pwned",
        }
        self.agent.handle_request(req)
        body = json.loads((self.fleet / "result-unsigned-cmd.json").read_text(encoding="utf-8"))
        self.assertFalse(body["ok"], body)
        stdout = body.get("stdout") or ""
        self.assertEqual(stdout, "", body)
        self.assertNotIn("pwned", stdout)

    def test_unsigned_tool_rejects_commands_override(self):
        self.agent.TOOLS_BY_JOB[self.job_id] = {
            "echo_ok": {"argv": ["echo", "safe"], "description": ""}
        }
        req = {
            "job_id": self.job_id,
            "action": "tool",
            "req_id": "unsigned-commands",
            "tool": "echo_ok",
            "cwd": str(self.cwd),
            "commands": ["echo pwned"],
        }
        self.agent.handle_request(req)
        body = json.loads((self.fleet / "result-unsigned-commands.json").read_text(encoding="utf-8"))
        self.assertFalse(body["ok"], body)
        stdout = body.get("stdout") or ""
        self.assertEqual(stdout, "", body)
        self.assertNotIn("pwned", stdout)

    def test_git_ref_rejects_shell_metachar(self):
        dest = self.tmp / "clone-metachar-ref"
        with self.assertRaises(ValueError) as cm:
            self.agent.git_clone(
                "https://x.example/r.git",
                'main"; curl evil',
                dest,
                os.environ.copy(),
            )
        self.assertIn("invalid git_ref", str(cm.exception))
        self.assertFalse(dest.exists())

    def test_git_clone_rejects_leading_dash_git_ref(self):
        dest = self.tmp / "clone-dash-ref"
        with self.assertRaises(ValueError) as cm:
            self.agent.git_clone(
                "https://x.example/r.git",
                "-evil",
                dest,
                os.environ.copy(),
            )
        self.assertIn("invalid git_ref", str(cm.exception))
        self.assertFalse(dest.exists())

    def test_git_clone_rejects_metachar_repo_url(self):
        dest = self.tmp / "clone-metachar-url"
        with self.assertRaises(ValueError) as cm:
            self.agent.git_clone(
                "https://x.example/r.git; curl evil",
                "main",
                dest,
                os.environ.copy(),
            )
        self.assertIn("invalid repo_url", str(cm.exception))
        self.assertFalse(dest.exists())

    def test_git_clone_rejects_newline_in_git_ref(self):
        dest = self.tmp / "clone-newline-ref"
        with self.assertRaises(ValueError) as cm:
            self.agent.git_clone(
                "https://x.example/r.git",
                "main\ncurl",
                dest,
                os.environ.copy(),
            )
        self.assertIn("invalid git_ref", str(cm.exception))
        self.assertFalse(dest.exists())

    def test_git_clone_rejects_nul_in_git_ref(self):
        dest = self.tmp / "clone-nul-ref"
        with self.assertRaises(ValueError) as cm:
            self.agent.git_clone(
                "https://x.example/r.git",
                "main\x00x",
                dest,
                os.environ.copy(),
            )
        self.assertIn("invalid git_ref", str(cm.exception))
        self.assertFalse(dest.exists())

    def test_handle_provision_invalid_git_ref_writes_failed_result(self):
        work_dir = self.jobs / "job-evil-ref" / "repo"
        req = self.sign(
            {
                "job_id": "job-evil-ref",
                "action": "provision",
                "req_id": "prov-evil-ref",
                "repo_url": "https://x.example/r.git",
                "git_ref": "-evil",
                "work_dir": str(work_dir),
                "commands": [],
                "smoke_commands": [],
                "env": {},
                "tools": {},
            }
        )
        self.agent.handle_request(req)
        body = json.loads((self.fleet / "result-prov-evil-ref.json").read_text(encoding="utf-8"))
        self.assertFalse(body.get("ok"), body)
        self.assertIn("invalid git_ref", body.get("error") or "")
        self.assertFalse(work_dir.exists(), "invalid git_ref must not clone work_dir")

    def _init_main_only_repo(self, name: str) -> Path:
        import subprocess

        src = self.tmp / name
        src.mkdir()
        (src / "README").write_text("only-main\n", encoding="utf-8")
        subprocess.run(["git", "init", "-b", "main", str(src)], check=True, capture_output=True)
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
        return src

    def test_handle_request_provision_missing_ref_writes_failed_result(self):
        src = self._init_main_only_repo("src-missing-ref")
        job_id = "job-missing-ref"
        work_dir = self.jobs / job_id / "repo"
        req = self.sign(
            {
                "job_id": job_id,
                "action": "provision",
                "req_id": "prov-missing-ref",
                "repo_url": str(src),
                "git_ref": "no-such-ref",
                "work_dir": str(work_dir),
                "commands": [],
                "smoke_commands": [],
                "env": {},
                "tools": {},
            }
        )
        self.agent.handle_request(req)
        body = json.loads((self.fleet / "result-prov-missing-ref.json").read_text(encoding="utf-8"))
        self.assertFalse(body.get("ok"), body)
        status = json.loads((self.fleet / "status.json").read_text(encoding="utf-8"))
        self.assertEqual(status.get("phase"), "failed", status)
        self.assertFalse(status.get("ok"), status)

    def test_handle_provision_missing_ref_does_not_emit_clone_done_or_register_tools(self):
        src = self._init_main_only_repo("src-missing-ref-tools")
        job_id = "job-missing-ref-tools"
        work_dir = self.jobs / job_id / "repo"
        with self.assertRaises(RuntimeError):
            self.agent.handle_provision(
                {
                    "job_id": job_id,
                    "repo_url": str(src),
                    "git_ref": "no-such-ref",
                    "work_dir": str(work_dir),
                    "commands": [],
                    "smoke_commands": [],
                    "env": {},
                    "tools": {
                        "echo_ok": {
                            "argv": ["echo", "should-not-register"],
                            "description": "",
                        }
                    },
                }
            )
        self.assertNotIn(job_id, self.agent.TOOLS_BY_JOB)
        status = json.loads((self.fleet / "status.json").read_text(encoding="utf-8"))
        self.assertNotEqual(status.get("phase"), "clone_done", status)
        self.assertNotIn("clone_done", status.get("milestones") or [])

    def test_handle_request_result_is_group_and_other_readable(self):
        req = self.sign(
            {
                "job_id": self.job_id,
                "action": "tool",
                "req_id": "mode-read",
                "tool": "echo_ok",
                "cwd": str(self.cwd),
            }
        )
        self.agent.handle_request(req)
        for name in ("result-mode-read.json", "status.json"):
            path = self.fleet / name
            mode = path.stat().st_mode
            self.assertEqual(
                mode & 0o044,
                0o044,
                f"{name} {oct(mode & 0o777)} missing group/other read",
            )

    def test_write_tools_allowlist_file_is_0444(self):
        work_dir = self.jobs / "job-tools-mode" / "repo"
        work_dir.mkdir(parents=True)
        self.agent.write_tools(
            work_dir,
            {"echo_ok": {"argv": ["echo", "tool-ok"], "description": "echo"}},
        )
        path = work_dir / ".fleet-tools.json"
        mode = path.stat().st_mode & 0o777
        self.assertEqual(mode, 0o444, oct(mode))

    def _restore_env(self, key: str, old: str | None) -> None:
        if old is None:
            os.environ.pop(key, None)
        else:
            os.environ[key] = old

    def test_git_ssh_env_sets_command_when_key_present(self):
        key_bytes = "dummy-key\n"
        key = self.tmp / "id_ed25519"
        key.write_text(key_bytes, encoding="utf-8")
        home = self.tmp / "home"
        home.mkdir()
        old_key = os.environ.get("GIT_SSH_KEY_PATH")
        old_home = os.environ.get("HOME")
        os.environ["GIT_SSH_KEY_PATH"] = str(key)
        os.environ["HOME"] = str(home)
        try:
            env = self.agent.git_ssh_env({})
            ident = home / ".ssh" / "id_fleet"
            config = home / ".ssh" / "config"
            self.assertEqual(env.get("GIT_SSH_COMMAND"), f"ssh -F {config}")
            self.assertTrue(ident.is_file())
            self.assertEqual(ident.read_text(encoding="utf-8"), key_bytes)
            self.assertEqual(ident.stat().st_mode & 0o777, 0o600)
            cfg = config.read_text(encoding="utf-8")
            self.assertIn(f"IdentityFile {ident}", cfg)
            self.assertIn("IdentitiesOnly yes", cfg)
        finally:
            self._restore_env("GIT_SSH_KEY_PATH", old_key)
            self._restore_env("HOME", old_home)

    def test_git_ssh_env_missing_key_leaves_env_unchanged(self):
        home = self.tmp / "home-missing-key"
        home.mkdir()
        old_key = os.environ.get("GIT_SSH_KEY_PATH")
        old_home = os.environ.get("HOME")
        os.environ["HOME"] = str(home)
        try:
            for label, key_val in (
                ("unset", None),
                ("nonexistent", str(self.tmp / "no-such-deploy-key")),
            ):
                with self.subTest(key=label):
                    if key_val is None:
                        os.environ.pop("GIT_SSH_KEY_PATH", None)
                    else:
                        os.environ["GIT_SSH_KEY_PATH"] = key_val
                    incoming = {"HOME": str(home)}
                    env = self.agent.git_ssh_env(incoming)
                    self.assertNotIn("GIT_SSH_COMMAND", env)
                    self.assertEqual(env, incoming)
                    self.assertFalse((home / ".ssh" / "id_fleet").exists())
        finally:
            self._restore_env("GIT_SSH_KEY_PATH", old_key)
            self._restore_env("HOME", old_home)

    def test_git_ssh_env_copies_known_hosts_when_set(self):
        key = self.tmp / "id_ed25519"
        key.write_text("dummy-key\n", encoding="utf-8")
        kh_bytes = "example.test ssh-ed25519 AAAA\n"
        kh = self.tmp / "known_hosts.src"
        kh.write_text(kh_bytes, encoding="utf-8")
        home = self.tmp / "home-known-hosts"
        home.mkdir()
        old_key = os.environ.get("GIT_SSH_KEY_PATH")
        old_home = os.environ.get("HOME")
        old_kh = os.environ.get("GIT_SSH_KNOWN_HOSTS")
        os.environ["GIT_SSH_KEY_PATH"] = str(key)
        os.environ["HOME"] = str(home)
        os.environ["GIT_SSH_KNOWN_HOSTS"] = str(kh)
        try:
            self.agent.git_ssh_env({})
            dest = home / ".ssh" / "known_hosts"
            self.assertTrue(dest.is_file())
            self.assertEqual(dest.read_text(encoding="utf-8"), kh_bytes)
        finally:
            self._restore_env("GIT_SSH_KEY_PATH", old_key)
            self._restore_env("HOME", old_home)
            self._restore_env("GIT_SSH_KNOWN_HOSTS", old_kh)

    def test_handle_request_refuses_when_handshake_secret_unset(self):
        """verify_request refuses provision when FLEET_HANDSHAKE_SECRET is unset/blank."""
        for label, secret in (("unset", None), ("blank", "   ")):
            with self.subTest(secret=label):
                fleet = self.tmp / f"fleet-secret-{label}"
                jobs = self.tmp / f"jobs-secret-{label}"
                fleet.mkdir()
                jobs.mkdir()
                agent = load_agent(fleet, jobs, handshake_secret=secret)
                work_dir = jobs / "secret-unset" / "repo"
                req = {
                    "job_id": "secret-unset",
                    "action": "provision",
                    "req_id": f"secret-{label}",
                    "repo_url": str(self.cwd),
                    "git_ref": "main",
                    "work_dir": str(work_dir),
                    "commands": [],
                    "smoke_commands": [],
                    "env": {},
                    "tools": {},
                }
                raw = agent.canonical_json(req).encode()
                req["hmac"] = hmac.new(b"signed-looking", raw, hashlib.sha256).hexdigest()
                agent.handle_request(req)
                body = json.loads(
                    (fleet / f"result-secret-{label}.json").read_text(encoding="utf-8")
                )
                self.assertFalse(body.get("ok"), body)
                err = f"{body.get('error') or ''} {body.get('stderr') or ''}".lower()
                self.assertIn("secret", err, body)
                self.assertFalse(work_dir.exists(), "unset secret must not create work_dir")

    def test_signed_tool_hmac_replay_is_rejected(self):
        """SEEN_HMACS one-shot applies to a signed tool request, not only provision."""
        marker = self.cwd / "tool-ran"
        self.agent.TOOLS_BY_JOB[self.job_id] = {
            "mark": {"argv": ["touch", "tool-ran"], "description": "touch marker"},
        }
        req = self.sign(
            {
                "job_id": self.job_id,
                "action": "tool",
                "req_id": "tool-replay",
                "tool": "mark",
                "cwd": str(self.cwd),
            }
        )
        self.agent.handle_request(req)
        first = json.loads(
            (self.fleet / "result-tool-replay.json").read_text(encoding="utf-8")
        )
        self.assertTrue(first.get("ok"), first)
        self.assertTrue(marker.is_file(), "first signed tool request must exec")
        marker.unlink()
        (self.fleet / "result-tool-replay.json").unlink()
        self.agent.handle_request(req)
        replay = json.loads(
            (self.fleet / "result-tool-replay.json").read_text(encoding="utf-8")
        )
        self.assertFalse(replay.get("ok"), replay)
        err = f"{replay.get('error') or ''} {replay.get('stderr') or ''}".lower()
        self.assertIn("replayed", err, replay)
        self.assertIn("hmac", err, replay)
        self.assertFalse(marker.exists(), "replay must not exec the tool again")


if __name__ == "__main__":
    unittest.main(verbosity=2)
