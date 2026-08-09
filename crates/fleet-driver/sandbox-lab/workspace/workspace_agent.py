#!/usr/bin/env python3
"""Workspace agent: provision + named allowlisted tools (no free shell)."""
from __future__ import annotations
import hashlib, hmac, json, os, shutil, subprocess, sys, time, traceback, uuid
from pathlib import Path

FLEET = Path(os.environ.get("FLEET_DIR", "/workspace/.fleet"))
POLL = float(os.environ.get("FLEET_WORKSPACE_POLL_SEC", "0.4"))
FLEET_UID = int(os.environ.get("FLEET_UID", "1000"))
FLEET_GID = int(os.environ.get("FLEET_GID", "1000"))
BIN = Path(os.environ.get("FLEET_BIN_DIR", "/workspace/bin"))
ALLOW_RAW = os.environ.get("FLEET_ALLOW_RAW_EXEC", "0") in ("1", "true", "yes")

# job_id -> tools map from the signed provision request (immutable vs harness file writes)
TOOLS_BY_JOB: dict[str, dict] = {}

ALLOWED_BINS = {
    "python", "python3", "pip", "pip3", "npm", "npx", "node", "pytest",
    "cargo", "go", "make", "uv", "poetry", "django-admin", "git",
    "ls", "cat", "head", "echo", "true", "false", "test", "mkdir", "cp", "touch",
}


def handshake_secret() -> str:
    return (os.environ.get("FLEET_HANDSHAKE_SECRET") or "").strip()


def canonical_json(obj) -> str:
    return json.dumps(obj, sort_keys=True, separators=(",", ":"))


def verify_request(req: dict) -> None:
    secret = handshake_secret()
    if not secret:
        raise ValueError("FLEET_HANDSHAKE_SECRET not set; refusing volume requests")
    got = req.get("hmac") or ""
    body = {k: v for k, v in req.items() if k != "hmac"}
    raw = canonical_json(body).encode()
    expect = hmac.new(secret.encode(), raw, hashlib.sha256).hexdigest()
    if not hmac.compare_digest(str(got), expect):
        raise ValueError("invalid handshake hmac")


def jobs_root() -> Path:
    """Job isolation root (default /workspace/jobs). Overridable for tests via FLEET_JOBS_ROOT."""
    raw = os.environ.get("FLEET_JOBS_ROOT", "/workspace/jobs")
    p = Path(raw)
    try:
        return p.resolve()
    except OSError:
        return p.absolute()


def path_under_jobs(path: Path, root: Path | None = None) -> bool:
    """True if path is under jobs isolation root (no '..', logical prefix check)."""
    root = root or jobs_root()
    for c in path.parts:
        if c == "..":
            return False
    try:
        resolved = path.resolve()
        root_r = root.resolve()
        resolved.relative_to(root_r)
        return True
    except (ValueError, OSError):
        pass
    try:
        root_s = str(root if root.is_absolute() else root.absolute())
        path_s = str(path if path.is_absolute() else path.absolute())
    except OSError:
        root_s = str(root)
        path_s = str(path)
    root_s = root_s.rstrip("/")
    path_s = path_s.rstrip("/")
    return path_s == root_s or path_s.startswith(root_s + "/")


def log(msg):
    print(f'[workspace-agent] {msg}', flush=True)


def write_json(path, obj, mode=0o640):
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + '.tmp')
    tmp.write_text(json.dumps(obj, indent=2) + '\n', encoding='utf-8')
    tmp.replace(path)
    try:
        os.chmod(path, mode)
    except Exception:
        pass


def status(job_id, phase, ok=True, message='', **extra):
    body = {'job_id': job_id, 'phase': phase, 'ok': ok, 'message': message, 'ts': time.time(), **extra}
    write_json(FLEET / 'status.json', body)
    log(f'status phase={phase} ok={ok} {message}')


def run_argv(argv, cwd, env):
    log(f'exec argv={argv} cwd={cwd}')
    proc = subprocess.run(argv, cwd=str(cwd), env=env, capture_output=True, text=True)
    out, err = proc.stdout or '', proc.stderr or ''
    if out:
        log(f'stdout: {out[-2000:]}')
    if proc.returncode != 0 and err:
        log(f'stderr: {err[-2000:]}')
    return proc.returncode, out, err


def run_cmd(cmd, cwd, env):
    """Provision-only shell (commands come from signed driver request)."""
    return run_argv(['bash', '-lc', cmd], cwd, env)


def chown_tree(path):
    if not path.exists():
        return
    try:
        subprocess.run(
            ['chown', '-R', f'{FLEET_UID}:{FLEET_GID}', str(path)],
            check=False, capture_output=True,
        )
    except Exception as e:
        log(f'chown warning: {e}')


def validate_git_arg(name, value):
    if value is None:
        raise ValueError(f"{name} required")
    s = str(value)
    if not s or s.startswith("-") or any(c in s for c in '"\'\n\r\x00;|&$`'):
        raise ValueError(f"invalid {name}")
    return s


def validate_tool_bin(argv0: str):
    base = Path(argv0).name
    if base not in ALLOWED_BINS:
        raise ValueError(f"argv[0] {argv0!r} (basename {base!r}) is not an allowlisted tool binary")


def install_fleet_wsexec():
    BIN.mkdir(parents=True, exist_ok=True)
    path = BIN / "fleet-wsexec"
    # Name-only handshake to the workspace agent. Argv is never taken from the
    # harness; agent runs TOOLS_BY_JOB. Wrapper is root-owned 0755.
    path.write_text(
        """#!/usr/bin/env python3
import json, os, sys, time
from pathlib import Path

if len(sys.argv) != 2:
    print("usage: fleet-wsexec <tool-name>", file=sys.stderr)
    raise SystemExit(2)
name = sys.argv[1]
cwd = os.environ.get("FLEET_WORK_DIR") or ""
job_id = os.environ.get("FLEET_JOB_ID") or "adhoc"
fleet = Path(os.environ.get("FLEET_DIR") or "/workspace/.fleet")
if not cwd or not os.path.isdir(cwd):
    print("fleet-wsexec: FLEET_WORK_DIR missing", file=sys.stderr)
    raise SystemExit(2)
tools_path = Path(cwd) / ".fleet-tools.json"
if tools_path.is_file():
    try:
        listed = json.loads(tools_path.read_text(encoding="utf-8"))
        if name not in listed:
            print(f"fleet-wsexec: tool {name!r} not allowlisted", file=sys.stderr)
            raise SystemExit(2)
    except SystemExit:
        raise
    except Exception:
        pass
req_id = f"{int(time.time())}-{os.getpid()}"
req = {
    "job_id": job_id,
    "action": "tool",
    "req_id": req_id,
    "tool": name,
    "cwd": cwd,
}
req_path = fleet / f"request-{req_id}.json"
res_path = fleet / f"result-{req_id}.json"
tmp = req_path.with_name(req_path.name + ".tmp")
tmp.write_text(json.dumps(req), encoding="utf-8")
tmp.replace(req_path)
for _ in range(6000):
    if res_path.is_file():
        d = json.loads(res_path.read_text(encoding="utf-8"))
        sys.stdout.write(d.get("stdout") or "")
        sys.stderr.write(d.get("stderr") or "")
        code = 0 if d.get("ok") else int(d.get("exit_code") or 1)
        raise SystemExit(code)
    time.sleep(0.1)
print("fleet-wsexec: timeout", file=sys.stderr)
raise SystemExit(124)
"""
    )
    os.chmod(path, 0o755)
    try:
        os.chown(BIN, 0, 0)
        os.chmod(BIN, 0o755)
        os.chown(path, 0, 0)
        os.chmod(path, 0o755)
    except Exception as e:
        log(f"fleet-wsexec perms warning: {e}")
    log(f"installed {path} (volume tool handshake, root-owned)")


def git_clone(repo_url, git_ref, dest, env):
    repo_url = validate_git_arg("repo_url", repo_url)
    git_ref = validate_git_arg("git_ref", git_ref or "main")
    dest = Path(dest)
    if dest.exists():
        log(f"removing existing {dest}")
        shutil.rmtree(dest)
    dest.parent.mkdir(parents=True, exist_ok=True)
    code, out, err = run_argv(
        ["git", "clone", "--depth", "1", "--branch", git_ref, repo_url, str(dest)],
        dest.parent,
        env,
    )
    if code != 0:
        log("clone with --branch failed; retry default branch")
        if dest.exists():
            shutil.rmtree(dest)
        code, out, err = run_argv(
            ["git", "clone", "--depth", "1", repo_url, str(dest)],
            dest.parent,
            env,
        )
        if code != 0:
            raise RuntimeError(f"git clone failed: {err}\n{out}")
        if git_ref:
            run_argv(["git", "checkout", git_ref], dest, env)


def write_tools(work_dir, tools):
    out = {}
    for name, spec in (tools or {}).items():
        if isinstance(spec, dict) and spec.get("argv"):
            argv = list(spec["argv"])
            validate_tool_bin(str(argv[0]))
            for a in argv:
                if not isinstance(a, str) or any(c in a for c in "\n\r;|&$`"):
                    raise ValueError(f"tool {name!r} argv rejected: {a!r}")
            out[name] = {"argv": argv, "description": spec.get("description") or ""}
    path = work_dir / ".fleet-tools.json"
    write_json(path, out, mode=0o444)
    try:
        os.chmod(path, 0o444)
    except Exception:
        pass
    lines = ["# Fleet workspace tools (allowlisted)", "", "Use only:", ""]
    for name, spec in sorted(out.items()):
        lines.append(f"- `fleet-wsexec {name}` -> `{' '.join(spec['argv'])}`")
        if spec.get("description"):
            lines.append(f"  - {spec['description']}")
    lines += ["", "No free shell into the workspace.", ""]
    (work_dir / "FLEET_TOOLS.md").write_text("\n".join(lines), encoding="utf-8")
    log(f"wrote {path} tools={list(out)} (0444)")
    return out


def handle_provision(req):
    job_id = req.get("job_id") or str(uuid.uuid4())
    repo_url = req.get("repo_url") or ""
    git_ref = req.get("git_ref") or "main"
    default_wd = jobs_root() / job_id / "repo"
    work_dir = Path(req.get("work_dir") or str(default_wd))
    if not path_under_jobs(work_dir):
        raise ValueError(
            f"work_dir outside job isolation: {work_dir} (must be under {jobs_root()})"
        )
    commands = req.get("commands") or []
    smoke = req.get("smoke_commands") or []
    tools = req.get("tools") or {}
    extra_env = req.get("env") or {}
    env = os.environ.copy()
    env.update({str(k): str(v) for k, v in extra_env.items()})
    env.setdefault("PIP_DISABLE_PIP_VERSION_CHECK", "1")
    env.setdefault("DEBIAN_FRONTEND", "noninteractive")
    milestones = []

    def mile(name):
        milestones.append(name)
        status(job_id, name, milestones=milestones)

    if not repo_url:
        raise ValueError("repo_url required")
    mile("clone_started")
    git_clone(repo_url, git_ref, work_dir, env)
    mile("clone_done")
    if commands:
        mile("deps_install_started")
        for cmd in commands:
            status(job_id, "deps_install_running", message=cmd, milestones=milestones)
            code, out, err = run_cmd(cmd, work_dir, env)
            if code != 0:
                raise RuntimeError(f"command failed ({code}): {cmd}\n{err}\n{out}")
        mile("deps_install_done")
    written = write_tools(work_dir, tools if isinstance(tools, dict) else {})
    TOOLS_BY_JOB[str(job_id)] = written
    if smoke:
        mile("smoke_started")
        for cmd in smoke:
            status(job_id, "smoke_running", message=cmd, milestones=milestones)
            code, out, err = run_cmd(cmd, work_dir, env)
            if code != 0:
                raise RuntimeError(f"smoke failed ({code}): {cmd}\n{err}\n{out}")
        mile("smoke_ok")
    chown_tree(work_dir)
    # Keep tools file root-owned / 0444 so the harness cannot rewrite argv.
    tools_path = work_dir / ".fleet-tools.json"
    try:
        os.chmod(tools_path, 0o444)
        os.chown(tools_path, 0, 0)
    except Exception:
        pass
    job_root = work_dir.parent if work_dir.name == "repo" else work_dir
    chown_tree(job_root)
    mile("ready")
    return {
        "ok": True,
        "job_id": job_id,
        "work_dir": str(work_dir),
        "repo_url": repo_url,
        "git_ref": git_ref,
        "milestones": milestones,
        "tools": list(written),
    }


def handle_tool(req, require_registered: bool = False):
    job_id = req.get("job_id") or "adhoc"
    tool = req.get("tool") or ""
    cwd = Path(req.get("cwd") or req.get("work_dir") or "")
    if not tool:
        raise ValueError("tool name required")
    if not cwd.is_dir():
        raise ValueError(f"cwd missing: {cwd}")
    resolved = cwd.resolve()
    if not path_under_jobs(cwd):
        raise ValueError(f"cwd outside job isolation: {resolved} (must be under {jobs_root()})")
    tools = TOOLS_BY_JOB.get(str(job_id))
    if tools is None:
        if require_registered:
            raise ValueError(
                f"unsigned tool request before provision (job {job_id!r} has no registered tools)"
            )
        tools_path = cwd / ".fleet-tools.json"
        if not tools_path.is_file():
            raise ValueError(f"no tools file at {tools_path}")
        tools = json.loads(tools_path.read_text(encoding="utf-8"))
    if tool not in tools:
        raise ValueError(f"tool {tool!r} not allowlisted; allowed={sorted(tools)}")
    argv = tools[tool].get("argv") or []
    if not argv or not isinstance(argv, list):
        raise ValueError(f"tool {tool!r} has invalid argv")
    validate_tool_bin(str(argv[0]))
    for a in argv:
        if not isinstance(a, str) or any(c in a for c in "\n\r;|&$`"):
            raise ValueError(f"tool {tool!r} argv rejected: {a!r}")
    env = os.environ.copy()
    status(job_id, "tool_running", message=tool)
    code, out, err = run_argv([str(x) for x in argv], cwd, env)
    ok = code == 0
    status(job_id, "tool_done" if ok else "tool_failed", message=tool, ok=ok)
    return {
        "ok": ok,
        "job_id": job_id,
        "tool": tool,
        "argv": argv,
        "cwd": str(cwd),
        "exit_code": code,
        "stdout": out[-100000:],
        "stderr": err[-50000:],
    }


def handle_exec_legacy(req):
    if not ALLOW_RAW:
        raise ValueError("raw exec disabled; use fleet-wsexec <tool-name>")
    job_id = req.get("job_id") or "adhoc"
    cmd = req.get("cmd") or req.get("command")
    cwd = Path(req.get("cwd") or "/workspace")
    if not cmd:
        raise ValueError("exec requires cmd")
    code, out, err = run_cmd(cmd, cwd, os.environ.copy())
    return {
        "ok": code == 0,
        "job_id": job_id,
        "cmd": cmd,
        "exit_code": code,
        "stdout": out[-100000:],
        "stderr": err[-50000:],
    }


def result_path(req):
    rid = req.get("req_id")
    if rid:
        return FLEET / f"result-{rid}.json"
    return FLEET / "result.json"


def handle_request(req):
    action = (req.get("action") or "provision").lower()
    job_id = req.get("job_id") or ""
    rpath = result_path(req)
    try:
        if action == "provision" or action == "exec":
            verify_request(req)
        elif action == "tool":
            if req.get("hmac"):
                verify_request(req)
            else:
                # Harness fleet-wsexec cannot hold the handshake secret.
                # Name-only: argv never taken from the request; TOOLS_BY_JOB only.
                if req.get("argv") or req.get("commands") or req.get("cmd"):
                    raise ValueError("unsigned tool request must not include argv/commands")
                if str(job_id) not in TOOLS_BY_JOB:
                    raise ValueError(
                        f"unsigned tool request before provision (job {job_id!r} has no registered tools)"
                    )
        else:
            raise ValueError(f"unknown action {action}")
        if action == "provision":
            result = handle_provision(req)
        elif action == "tool":
            result = handle_tool(req, require_registered=not bool(req.get("hmac")))
        elif action == "exec":
            result = handle_exec_legacy(req)
        else:
            raise ValueError(f"unknown action {action}")
        write_json(rpath, result)
        status(job_id, "done", ok=True, message="ok")
    except Exception as e:
        log(f"FAILED: {e}\n{traceback.format_exc()}")
        err = {"ok": False, "job_id": job_id, "error": str(e), "exit_code": 1, "stdout": "", "stderr": str(e)}
        write_json(rpath, err)
        status(job_id, "failed", ok=False, message=str(e))


def ensure_shared_permissions():
    root = Path("/workspace")
    jobs = Path("/workspace/jobs")
    root.mkdir(parents=True, exist_ok=True)
    FLEET.mkdir(parents=True, exist_ok=True)
    jobs.mkdir(parents=True, exist_ok=True)
    try:
        os.chmod(root, 0o755)
        os.chmod(jobs, 0o775)
        # Driver (uid 1000) + agent (root) share group 1000; others cannot write.
        os.chown(FLEET, 0, FLEET_GID)
        os.chmod(FLEET, 0o770)
    except Exception as e:
        log(f"chmod warning: {e}")
    try:
        os.chown(jobs, FLEET_UID, FLEET_GID)
    except Exception:
        pass


def pick_request():
    if not FLEET.is_dir():
        return None
    reqs = sorted(FLEET.glob("request-*.json"))
    if reqs:
        return reqs[0]
    return None


def main():
    ensure_shared_permissions()
    install_fleet_wsexec()
    for pth in FLEET.glob("request*.json"):
        try:
            pth.unlink()
        except Exception:
            pass
    try:
        (FLEET / "agent.alive").write_text(str(time.time()), encoding="utf-8")
        os.chmod(FLEET / "agent.alive", 0o640)
    except Exception as e:
        log(f"alive marker warning: {e}")
    log(f"listening for signed requests in {FLEET}")
    while True:
        req_path = pick_request()
        if req_path is None:
            time.sleep(POLL)
            continue
        try:
            req = json.loads(req_path.read_text(encoding="utf-8"))
        except Exception as e:
            log(f"bad request {req_path.name}: {e}")
            try:
                req_path.unlink()
            except Exception:
                pass
            continue
        claimed = req_path.with_suffix(".claimed")
        try:
            req_path.replace(claimed)
        except Exception:
            time.sleep(POLL)
            continue
        log(f"handling action={req.get('action')} job={req.get('job_id')}")
        handle_request(req)
        try:
            claimed.unlink(missing_ok=True)
        except Exception:
            pass


if __name__ == "__main__":
    raise SystemExit(main())
