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
from pathlib import Path

SECRET = "e2e-handshake-secret"
HERE = Path(__file__).resolve().parent
AGENT = HERE.parent / "workspace" / "workspace_agent.py"
BUGS: list[str] = []
OKS: list[str] = []


def note_ok(msg: str) -> None:
    OKS.append(msg)
    print(f"  OK  {msg}")


def note_bug(msg: str) -> None:
    BUGS.append(msg)
    print(f"  BUG {msg}")


def sign(req: dict) -> dict:
    body = {k: v for k, v in req.items() if k != "hmac"}
    raw = json.dumps(body, sort_keys=True, separators=(",", ":")).encode()
    body["hmac"] = hmac.new(SECRET.encode(), raw, hashlib.sha256).hexdigest()
    return body


def wait_file(path: Path, timeout: float = 20.0) -> dict | None:
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


def main() -> int:
    if not AGENT.is_file():
        print(f"missing workspace agent at {AGENT}", file=sys.stderr)
        return 2

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
    subprocess.run(["git", "-C", str(src), "config", "user.email", "e2e@example.test"], check=True)
    subprocess.run(["git", "-C", str(src), "config", "user.name", "e2e"], check=True)
    subprocess.run(["git", "-C", str(src), "add", "README"], check=True)
    subprocess.run(["git", "-C", str(src), "commit", "-q", "-m", "init"], check=True)
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
    print(f"temp={tmp}")
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
            note_bug("agent.alive or fleet-wsexec missing")
            print(log_path.read_text(encoding="utf-8")[-2000:])
            return 1
        note_ok("agent.alive + fleet-wsexec appeared")

        wsexec = bindir / "fleet-wsexec"
        st = wsexec.stat()
        if st.st_mode & 0o022:
            note_bug(f"fleet-wsexec is group/other-writable: mode={oct(st.st_mode)}")
        else:
            note_ok("fleet-wsexec not group/other-writable")

        job_id = "e2e-job-1"
        work_dir = jobs / job_id / "repo"
        req_id = "prov-1"
        provision = sign(
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
                    "show_readme": {"argv": ["cat", "README"], "description": "cat"},
                    "show_marker": {"argv": ["cat", "docs/marker.txt"], "description": "marker"},
                },
            }
        )
        (fleet / f"request-{req_id}.json").write_text(json.dumps(provision), encoding="utf-8")
        result = wait_file(fleet / f"result-{req_id}.json", 30)
        if not result:
            note_bug("provision timed out")
            print(log_path.read_text(encoding="utf-8")[-2000:])
            return 1
        if not result.get("ok"):
            note_bug(f"provision failed: {result.get('error')}")
        else:
            note_ok(f"provision ok milestones={result.get('milestones')}")
        if not (work_dir / "README").is_file():
            note_bug("clone did not produce README")
        else:
            note_ok("cloned README present")
        if not (work_dir / "docs" / "marker.txt").is_file():
            note_bug("provision command did not write docs/marker.txt")
        else:
            note_ok("provision shell command wrote marker")

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
        if p.returncode == 0 and "e2e-hello" in p.stdout:
            note_ok("fleet-wsexec hello")
        else:
            note_bug(f"fleet-wsexec hello rc={p.returncode} err={p.stderr!r}")

        p = subprocess.run(
            [str(wsexec), "show_marker"],
            cwd=str(work_dir),
            env=child_env,
            capture_output=True,
            text=True,
            timeout=20,
        )
        if p.returncode == 0 and "provisioned" in p.stdout:
            note_ok("fleet-wsexec show_marker")
        else:
            note_bug(f"show_marker rc={p.returncode} err={p.stderr!r}")

        p = subprocess.run(
            [str(wsexec), "not_a_tool"],
            cwd=str(work_dir),
            env=child_env,
            capture_output=True,
            text=True,
            timeout=20,
        )
        if p.returncode != 0:
            note_ok("unknown tool rejected")
        else:
            note_bug("unknown tool unexpectedly succeeded")

        tools_path = work_dir / ".fleet-tools.json"
        try:
            tools_path.write_text(
                json.dumps({"hello": {"argv": ["echo", "from-file-mutation"]}}),
                encoding="utf-8",
            )
        except PermissionError:
            note_ok("in-place rewrite of 0444 .fleet-tools.json denied")
            tools_path.unlink()
            tools_path.write_text(
                json.dumps({"hello": {"argv": ["echo", "from-file-mutation"]}}),
                encoding="utf-8",
            )
            note_ok("unlink+replace of tools file possible (dir writable)")

        p = subprocess.run(
            [str(wsexec), "hello"],
            cwd=str(work_dir),
            env=child_env,
            capture_output=True,
            text=True,
            timeout=20,
        )
        if "e2e-hello" in p.stdout and "from-file-mutation" not in p.stdout:
            note_ok("mutated .fleet-tools.json ignored (TOOLS_BY_JOB)")
        else:
            note_bug(f"tools file mutation leaked: out={p.stdout!r}")

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
        bad = wait_file(fleet / f"result-{bad_id}.json")
        if bad and not bad.get("ok") and "argv" in (bad.get("error") or "").lower():
            note_ok("unsigned argv override rejected")
        else:
            note_bug(f"unsigned argv override not rejected: {bad}")

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
        fake = wait_file(fleet / f"result-{fake_id}.json")
        if fake and not fake.get("ok"):
            note_ok("unsigned provision rejected")
        else:
            note_bug(f"unsigned provision accepted: {fake}")

        p = subprocess.run(
            [str(wsexec), "show_readme"],
            cwd=str(work_dir),
            env=child_env,
            capture_output=True,
            text=True,
            timeout=20,
        )
        if p.returncode == 0 and "hello-from-src" in p.stdout:
            note_ok("fleet-wsexec cat README from cloned repo")
        else:
            note_bug(f"show_readme failed rc={p.returncode} err={p.stderr!r}")

    finally:
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        logf.close()
        shutil.rmtree(tmp, ignore_errors=True)

    print("\n===== SUMMARY =====")
    print(f"ok={len(OKS)} bugs={len(BUGS)}")
    for b in BUGS:
        print(f" - {b}")
    return 1 if BUGS else 0


if __name__ == "__main__":
    raise SystemExit(main())
