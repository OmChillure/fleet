#!/usr/bin/env python3
"""Workspace agent: provision + named allowlisted tools (no free shell)."""
from __future__ import annotations
import json, os, shutil, subprocess, sys, time, traceback, uuid
from pathlib import Path

FLEET = Path(os.environ.get("FLEET_DIR", "/workspace/.fleet"))
POLL = float(os.environ.get("FLEET_WORKSPACE_POLL_SEC", "0.4"))
FLEET_UID = int(os.environ.get("FLEET_UID", "1000"))
FLEET_GID = int(os.environ.get("FLEET_GID", "1000"))
BIN = Path(os.environ.get("FLEET_BIN_DIR", "/workspace/bin"))
ALLOW_RAW = os.environ.get("FLEET_ALLOW_RAW_EXEC", "0") in ("1", "true", "yes")


def jobs_root() -> Path:
    """Job isolation root (default /workspace/jobs). Overridable for tests via FLEET_JOBS_ROOT."""
    raw = os.environ.get("FLEET_JOBS_ROOT", "/workspace/jobs")
    p = Path(raw)
    try:
        return p.resolve()
    except OSError:
        return p.absolute()


def path_under_jobs(path: Path, root: Path | None = None) -> bool:
    """True if path is under jobs isolation root (no '..', logical prefix check).

    Works for paths that do not exist yet (provision). Rejects ParentDir components.
    """
    root = root or jobs_root()
    for c in path.parts:
        if c == "..":
            return False
    # Prefer resolve when possible
    try:
        resolved = path.resolve()
        root_r = root.resolve()
        resolved.relative_to(root_r)
        return True
    except (ValueError, OSError):
        pass
    # Fallback: absolute string prefix (for not-yet-created parents)
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

def write_json(path, obj):
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + '.tmp')
    tmp.write_text(json.dumps(obj, indent=2) + '\n', encoding='utf-8')
    tmp.replace(path)
    try: os.chmod(path, 0o666)
    except Exception: pass

def status(job_id, phase, ok=True, message='', **extra):
    body = {'job_id': job_id, 'phase': phase, 'ok': ok, 'message': message, 'ts': time.time(), **extra}
    write_json(FLEET / 'status.json', body)
    log(f'status phase={phase} ok={ok} {message}')

def run_argv(argv, cwd, env):
    log(f'exec argv={argv} cwd={cwd}')
    proc = subprocess.run(argv, cwd=str(cwd), env=env, capture_output=True, text=True)
    out, err = proc.stdout or '', proc.stderr or ''
    if out: log(f'stdout: {out[-2000:]}')
    if proc.returncode != 0 and err: log(f'stderr: {err[-2000:]}')
    return proc.returncode, out, err

def run_cmd(cmd, cwd, env):
    return run_argv(['bash', '-lc', cmd], cwd, env)

def chown_tree(path):
    if not path.exists(): return
    try:
        subprocess.run(['chown', '-R', f'{FLEET_UID}:{FLEET_GID}', str(path)], check=False, capture_output=True)
    except Exception as e:
        log(f'chown warning: {e}')


def install_fleet_wsexec():
    BIN.mkdir(parents=True, exist_ok=True)
    path = BIN / "fleet-wsexec"
    # Named tools only
    path.write_text("""#!/usr/bin/env bash
set -euo pipefail
FLEET_DIR="${FLEET_DIR:-/workspace/.fleet}"
CWD="${FLEET_WORK_DIR:-}"
if [[ $# -ne 1 ]]; then
  echo "usage: fleet-wsexec <tool-name>" >&2
  exit 2
fi
TOOL="$1"
if [[ -z "$CWD" || ! -d "$CWD" ]]; then
  echo "fleet-wsexec: FLEET_WORK_DIR missing" >&2
  exit 2
fi
TOOLS_FILE="$CWD/.fleet-tools.json"
if [[ ! -f "$TOOLS_FILE" ]]; then
  echo "fleet-wsexec: missing $TOOLS_FILE" >&2
  exit 2
fi
python3 -c "import json,sys; t=json.load(open(sys.argv[1])); n=sys.argv[2];
assert n in t and t[n].get('argv'), (n, sorted(t))" "$TOOLS_FILE" "$TOOL"
REQ_ID="$(date +%s)-$RANDOM"
REQ="$FLEET_DIR/request-${REQ_ID}.json"
RES="$FLEET_DIR/result-${REQ_ID}.json"
python3 -c "import json,os; open('$REQ','w').write(json.dumps({'job_id':os.environ.get('FLEET_JOB_ID','adhoc'),'action':'tool','req_id':'$REQ_ID','tool':'$TOOL','cwd':'$CWD'}))"
for _ in $(seq 1 6000); do
  if [[ -f "$RES" ]]; then
    python3 -c "import json,sys; d=json.load(open(sys.argv[1])); import sys as S; S.stdout.write(d.get('stdout') or ''); S.stderr.write(d.get('stderr') or ''); raise SystemExit(0 if d.get('ok') else int(d.get('exit_code') or 1))" "$RES"
    exit $?
  fi
  sleep 0.1
done
echo "fleet-wsexec: timeout" >&2
exit 124
""")
    os.chmod(path, 0o755)
    try: os.chmod(BIN, 0o755)
    except Exception: pass
    chown_tree(BIN)
    log(f"installed {path} (named tools only)")

def git_clone(repo_url, git_ref, dest, env):
    if dest.exists():
        log(f"removing existing {dest}")
        shutil.rmtree(dest)
    dest.parent.mkdir(parents=True, exist_ok=True)
    code, out, err = run_cmd(f'git clone --depth 1 --branch "{git_ref}" "{repo_url}" "{dest}"', dest.parent, env)
    if code != 0:
        log("clone with --branch failed; retry default branch")
        if dest.exists(): shutil.rmtree(dest)
        code, out, err = run_cmd(f'git clone --depth 1 "{repo_url}" "{dest}"', dest.parent, env)
        if code != 0:
            raise RuntimeError(f"git clone failed: {err}\n{out}")
        if git_ref:
            run_cmd(f'git checkout "{git_ref}" || true', dest, env)

def write_tools(work_dir, tools):
    out = {}
    for name, spec in (tools or {}).items():
        if isinstance(spec, dict) and spec.get("argv"):
            out[name] = {"argv": list(spec["argv"]), "description": spec.get("description") or ""}
    path = work_dir / ".fleet-tools.json"
    write_json(path, out)
    lines = ["# Fleet workspace tools (allowlisted)", "", "Use only:", ""]
    for name, spec in sorted(out.items()):
        lines.append(f"- `fleet-wsexec {name}` -> `{' '.join(spec['argv'])}`")
        if spec.get("description"):
            lines.append(f"  - {spec['description']}")
    lines += ["", "No free shell into the workspace.", ""]
    (work_dir / "FLEET_TOOLS.md").write_text("\n".join(lines), encoding="utf-8")
    log(f"wrote {path} tools={list(out)}")


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
    write_tools(work_dir, tools if isinstance(tools, dict) else {})
    if smoke:
        mile("smoke_started")
        for cmd in smoke:
            status(job_id, "smoke_running", message=cmd, milestones=milestones)
            code, out, err = run_cmd(cmd, work_dir, env)
            if code != 0:
                raise RuntimeError(f"smoke failed ({code}): {cmd}\n{err}\n{out}")
        mile("smoke_ok")
    chown_tree(work_dir)
    job_root = work_dir.parent if work_dir.name == "repo" else work_dir
    chown_tree(job_root)
    chown_tree(FLEET)
    mile("ready")
    return {"ok": True, "job_id": job_id, "work_dir": str(work_dir), "repo_url": repo_url, "git_ref": git_ref, "milestones": milestones, "tools": list(tools.keys()) if isinstance(tools, dict) else []}

def handle_tool(req):
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
    tools_path = cwd / ".fleet-tools.json"
    if not tools_path.is_file():
        raise ValueError(f"no tools file at {tools_path}")
    tools = json.loads(tools_path.read_text(encoding="utf-8"))
    if tool not in tools:
        raise ValueError(f"tool {tool!r} not allowlisted; allowed={sorted(tools)}")
    argv = tools[tool].get("argv") or []
    if not argv or not isinstance(argv, list):
        raise ValueError(f"tool {tool!r} has invalid argv")
    for a in argv:
        if not isinstance(a, str) or any(c in a for c in "\n\r;|&$`"):
            raise ValueError(f"tool {tool!r} argv rejected: {a!r}")
    env = os.environ.copy()
    status(job_id, "tool_running", message=tool)
    code, out, err = run_argv([str(x) for x in argv], cwd, env)
    ok = code == 0
    status(job_id, "tool_done" if ok else "tool_failed", message=tool, ok=ok)
    return {"ok": ok, "job_id": job_id, "tool": tool, "argv": argv, "cwd": str(cwd), "exit_code": code, "stdout": out[-100000:], "stderr": err[-50000:]}

def handle_exec_legacy(req):
    if not ALLOW_RAW:
        raise ValueError("raw exec disabled; use fleet-wsexec <tool-name>")
    job_id = req.get("job_id") or "adhoc"
    cmd = req.get("cmd") or req.get("command")
    cwd = Path(req.get("cwd") or "/workspace")
    if not cmd:
        raise ValueError("exec requires cmd")
    code, out, err = run_cmd(cmd, cwd, os.environ.copy())
    return {"ok": code == 0, "job_id": job_id, "cmd": cmd, "exit_code": code, "stdout": out[-100000:], "stderr": err[-50000:]}

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
        if action == "provision":
            result = handle_provision(req)
        elif action == "tool":
            result = handle_tool(req)
        elif action == "exec":
            result = handle_exec_legacy(req)
        else:
            raise ValueError(f"unknown action {action}")
        write_json(rpath, result)
        if action == "provision":
            write_json(FLEET / "result.json", result)
        status(job_id, "done", ok=True, message="ok")
    except Exception as e:
        log(f"FAILED: {e}\n{traceback.format_exc()}")
        err = {"ok": False, "job_id": job_id, "error": str(e), "exit_code": 1, "stdout": "", "stderr": str(e)}
        write_json(rpath, err)
        if action == "provision":
            write_json(FLEET / "result.json", err)
        status(job_id, "failed", ok=False, message=str(e))

def ensure_shared_permissions():
    root = Path("/workspace")
    root.mkdir(parents=True, exist_ok=True)
    FLEET.mkdir(parents=True, exist_ok=True)
    Path("/workspace/jobs").mkdir(parents=True, exist_ok=True)
    try:
        os.chmod(root, 0o777)
        os.chmod(FLEET, 0o777)
        os.chmod(Path("/workspace/jobs"), 0o777)
    except Exception as e:
        log(f"chmod warning: {e}")
    chown_tree(root)
    try: os.chmod(FLEET, 0o777)
    except Exception: pass

def pick_request():
    if not FLEET.is_dir():
        return None
    reqs = sorted(FLEET.glob("request-*.json"))
    if reqs:
        return reqs[0]
    legacy = FLEET / "request.json"
    return legacy if legacy.is_file() else None

def main():
    ensure_shared_permissions()
    install_fleet_wsexec()
    for pth in FLEET.glob("request*.json"):
        try: pth.unlink()
        except Exception: pass
    try:
        (FLEET / "agent.alive").write_text(str(time.time()), encoding="utf-8")
        os.chmod(FLEET / "agent.alive", 0o666)
    except Exception as e:
        log(f"alive marker warning: {e}")
    log(f"listening for requests in {FLEET}")
    while True:
        req_path = pick_request()
        if req_path is None:
            time.sleep(POLL)
            continue
        try:
            req = json.loads(req_path.read_text(encoding="utf-8"))
        except Exception as e:
            log(f"bad request {req_path.name}: {e}")
            try: req_path.unlink()
            except Exception: pass
            continue
        claimed = req_path.with_suffix(".claimed")
        try:
            req_path.replace(claimed)
        except Exception:
            time.sleep(POLL)
            continue
        log(f"handling action={req.get('action')} job={req.get('job_id')}")
        handle_request(req)
        try: claimed.unlink(missing_ok=True)
        except Exception: pass

if __name__ == "__main__":
    raise SystemExit(main())
