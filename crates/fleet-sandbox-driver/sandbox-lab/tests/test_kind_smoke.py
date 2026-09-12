#!/usr/bin/env python3
"""Live kind cluster smoke. Skips when cluster is not installed."""
from __future__ import annotations

import http.cookiejar
import json
import os
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid
import unittest
from pathlib import Path

_REPO = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(_REPO / "tests" / "browser"))
from stack_identity import ForeignStackError, require_this_tree  # noqa: E402

_LAB_TESTS = Path(__file__).resolve().parent
sys.path.insert(0, str(_LAB_TESTS))
from lab_env import env_flag  # noqa: E402


def _kubectl(*args: str) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    # Do not inherit a leaked HOME from gitssh live tests.
    kube = Path.home() / ".kube" / "config"
    if not kube.is_file():
        kube = Path(os.path.expanduser("~")) / ".kube" / "config"
    if kube.is_file():
        env["KUBECONFIG"] = str(kube)
    ctx = os.environ.get("KIND_KUBE_CONTEXT", "kind-agent-fleet")
    cmd = ["kubectl"]
    if ctx:
        cmd.extend(["--context", ctx])
    cmd.extend(args)
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        timeout=30,
        env=env,
    )


def _cluster() -> bool:
    """True only for a live agent-fleet ns (kind or equivalent), not host cargo run."""
    if not shutil.which("kubectl"):
        return False
    ns = _kubectl("get", "ns", "agent-fleet", "-o", "name")
    if ns.returncode != 0 or "agent-fleet" not in (ns.stdout or ""):
        return False
    if shutil.which("kind"):
        try:
            k = subprocess.run(
                ["kind", "get", "clusters"],
                capture_output=True,
                text=True,
                timeout=15,
            )
        except (OSError, subprocess.TimeoutExpired):
            return False
        if k.returncode != 0:
            return False
        names = {ln.strip() for ln in k.stdout.splitlines() if ln.strip()}
        if not names:
            return False
    return True


def _deploy_env(name: str) -> str:
    r = _kubectl(
        "-n",
        "agent-fleet",
        "get",
        "deploy",
        "api",
        "-o",
        f"jsonpath={{.spec.template.spec.containers[0].env[?(@.name==\"{name}\")].value}}",
    )
    return (r.stdout or "").strip() if r.returncode == 0 else ""


_LABEL_PROJECT = "fleet.agentfleet.io/project-id"


def _json_request(
    opener: urllib.request.OpenerDirector,
    method: str,
    url: str,
    body: dict | None = None,
    timeout: int = 15,
) -> tuple[int, dict | list | None, str]:
    data = None
    headers = {"Accept": "application/json"}
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with opener.open(req, timeout=timeout) as resp:
            raw = resp.read().decode()
            parsed: dict | list | None
            try:
                parsed = json.loads(raw) if raw else None
            except json.JSONDecodeError:
                parsed = None
            return resp.status, parsed, raw
    except urllib.error.HTTPError as e:
        raw = e.read().decode() if e.fp else ""
        parsed = None
        try:
            parsed = json.loads(raw) if raw else None
        except json.JSONDecodeError:
            parsed = None
        return e.code, parsed, raw
    except (urllib.error.URLError, TimeoutError) as e:
        raise AssertionError(f"{method} {url}: {e}") from e


def _fsb_names(project_id: str) -> list[str]:
    r = _kubectl(
        "-n",
        "agent-fleet",
        "get",
        "fsb",
        "-l",
        f"{_LABEL_PROJECT}={project_id}",
        "-o",
        "json",
    )
    if r.returncode != 0:
        raise RuntimeError(f"kubectl get fsb: {(r.stderr or r.stdout or '')[:300]}")
    data = json.loads(r.stdout or "{}")
    items = data.get("items") or []
    out: list[str] = []
    for item in items:
        name = ((item or {}).get("metadata") or {}).get("name") or ""
        if name:
            out.append(name)
    return out


def _poll_project_fsb(project_id: str, timeout_s: float, want_any: bool) -> list[str]:
    deadline = time.monotonic() + timeout_s
    last: list[str] = []
    last_err = ""
    while time.monotonic() < deadline:
        try:
            last = _fsb_names(project_id)
            last_err = ""
        except RuntimeError as e:
            last_err = str(e)
            time.sleep(1.5)
            continue
        if want_any and last:
            return last
        if not want_any and not last:
            return []
        time.sleep(1.5)
    if last_err:
        raise RuntimeError(last_err)
    return last


def _labeled_names(kind: str, project_id: str) -> list[str]:
    r = _kubectl(
        "-n",
        "agent-fleet",
        "get",
        kind,
        "-l",
        f"{_LABEL_PROJECT}={project_id}",
        "-o",
        "jsonpath={.items[*].metadata.name}",
    )
    if r.returncode != 0:
        raise RuntimeError(
            f"kubectl get {kind}: {(r.stderr or r.stdout or '')[:300]}"
        )
    return [n for n in (r.stdout or "").split() if n]


class KindSmokeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if _cluster():
            return
        if env_flag("ENABLE_KIND"):
            raise AssertionError(
                "ENABLE_KIND=1 but kind/agent-fleet namespace is not available"
            )
        raise unittest.SkipTest("kind/agent-fleet namespace not available")

    def setUp(self):
        kube = Path.home() / ".kube" / "config"
        self.assertTrue(
            kube.is_file(),
            f"no kubeconfig at {kube} (HOME={Path.home()}); "
            "a prior test leaked HOME — kubectl would hit :8080 (fleet-server)",
        )

    def test_control_plane_deployments_ready(self):
        try:
            require_this_tree(_REPO, "http://127.0.0.1:8080", require_ui=True)
        except ForeignStackError as e:
            self.fail(f"host :8080/:3030 is not this tree: {e}")
        # api/ui: HTTP identity. fleet-operator/fleet-host: tick + sandbox pod.
        for name in ("postgres", "zenohd"):
            deadline = time.monotonic() + 90
            last = ""
            while time.monotonic() < deadline:
                r = _kubectl(
                    "-n",
                    "agent-fleet",
                    "rollout",
                    "status",
                    f"deploy/{name}",
                    "--timeout=15s",
                )
                if r.returncode == 0:
                    break
                last = (r.stderr or r.stdout or "").strip()
                time.sleep(1.5)
            else:
                self.fail(f"kubectl rollout status deploy/{name}: {last[:200]}")

    def test_kind_zenohd_listens_7447(self):
        try:
            require_this_tree(_REPO, "http://127.0.0.1:8080", require_ui=True)
        except ForeignStackError as e:
            self.fail(f"host :8080/:3030 is not this tree: {e}")
        locator = _deploy_env("ZENOH_LOCATOR")
        self.assertEqual(
            locator,
            "tcp/zenohd.agent-fleet.svc.cluster.local:7447",
            f"api ZENOH_LOCATOR={locator!r}",
        )
        deadline = time.monotonic() + 60
        last = ""
        while time.monotonic() < deadline:
            probe = _kubectl(
                "-n",
                "agent-fleet",
                "exec",
                "deploy/api",
                "--",
                "timeout",
                "5",
                "bash",
                "-c",
                "echo >/dev/tcp/zenohd.agent-fleet.svc.cluster.local/7447",
            )
            if probe.returncode == 0:
                return
            last = (probe.stderr or probe.stdout or "").strip()
            time.sleep(1.5)
        self.fail(f"in-cluster connect {locator} from deploy/api: {last[:300]}")

    def test_host_ui(self):
        # Kind extraPortMappings 30303→3030, not FLEET_UI_URL (host remap :13030).
        try:
            with urllib.request.urlopen("http://127.0.0.1:3030/", timeout=5) as resp:
                self.assertEqual(resp.status, 200)
                root_ctype = (resp.headers.get("Content-Type") or "").lower()
                root = resp.read().decode().strip()
            with urllib.request.urlopen(
                "http://127.0.0.1:3030/fleet-git-sha.txt", timeout=5
            ) as resp:
                self.assertEqual(resp.status, 200)
                sha_ctype = (resp.headers.get("Content-Type") or "").lower()
                sha = resp.read().decode().strip()
        except (urllib.error.URLError, TimeoutError) as e:
            self.fail(f"host :3030: {e}")
        self.assertTrue(
            "html" in root_ctype
            or root[:15].lower().startswith(("<!doctype", "<html")),
            f"Kind :3030 / is not HTML (ctype={root_ctype!r} body={root[:80]!r})",
        )
        self.assertFalse(
            "html" in sha_ctype
            or sha[:15].lower().startswith(("<!doctype", "<html")),
            f"Kind :3030 /fleet-git-sha.txt is SPA fallback "
            f"(ctype={sha_ctype!r} body={sha[:80]!r})",
        )
        want = subprocess.check_output(
            ["bash", str(_REPO / "scripts" / "tree-id.sh")],
            text=True,
            cwd=str(_REPO),
        ).strip()
        self.assertEqual(sha, want, f"UI /fleet-git-sha.txt={sha!r} != tree-id {want}")

    def test_tick_creates_and_cancel_deletes_fleetsandbox(self):
        try:
            require_this_tree(_REPO, "http://127.0.0.1:8080", require_ui=True)
        except ForeignStackError as e:
            self.fail(
                f"host :8080/:3030 is not this tree: {e}. "
                "Rebuild + apply before treating kind smoke as valid."
            )

        jar = http.cookiejar.CookieJar()
        opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
        email = f"kind-k8s-{uuid.uuid4().hex[:12]}@example.test"
        password = "kind-pass-ok"
        st, body, raw = _json_request(
            opener,
            "POST",
            "http://127.0.0.1:8080/api/auth/signup",
            {
                "email": email,
                "password": password,
                "display_name": "kind-k8s",
            },
        )
        self.assertIn(st, {200, 201}, f"signup {st}: {raw[:400]}")
        st, body, raw = _json_request(
            opener,
            "POST",
            "http://127.0.0.1:8080/api/auth/login",
            {"email": email, "password": password},
        )
        self.assertIn(st, {200, 201}, f"login {st}: {raw[:400]}")
        self.assertIsInstance(body, dict)
        ws = str(((body or {}).get("default_workspace") or {}).get("id") or "")
        if not ws:
            st, me, raw = _json_request(opener, "GET", "http://127.0.0.1:8080/api/auth/me")
            self.assertEqual(st, 200, f"me {st}: {raw[:400]}")
            spaces = (me or {}).get("workspaces") or []
            ws = str((spaces[0].get("id") if spaces else "") or "")
        self.assertTrue(ws, "login/signup returned no workspace id")

        fleet = f"http://127.0.0.1:8080/api/workspaces/{ws}/fleet"
        deadline = time.monotonic() + 90
        host_online = False
        while time.monotonic() < deadline:
            st, hosts, raw = _json_request(
                opener, "GET", f"http://127.0.0.1:8080/api/workspaces/{ws}/worker-hosts"
            )
            self.assertEqual(st, 200, f"hosts {st}: {raw[:400]}")
            rows = (hosts or {}).get("hosts") or []
            host_online = any(
                isinstance(h, dict)
                and h.get("kind") != "control_plane"
                and str(h.get("status") or "").lower() == "online"
                for h in rows
            )
            if host_online:
                break
            time.sleep(2)
        self.assertTrue(
            host_online,
            "cluster fleet-host did not bootstrap-pair after signup",
        )

        st, fx, raw = _json_request(
            opener, "POST", f"{fleet}/runs/fixture", {"kind": "chain"}
        )
        self.assertEqual(st, 201, f"fixture {st}: {raw[:400]}")
        self.assertIsInstance(fx, dict)
        run_id = str((fx or {}).get("run_id") or "")
        self.assertTrue(run_id, f"fixture missing run_id: {fx}")

        st, _tick, raw = _json_request(opener, "POST", f"{fleet}/tick", timeout=60)
        self.assertEqual(st, 200, f"tick {st}: {raw[:400]}")

        names = _poll_project_fsb(ws, 90, want_any=True)
        self.assertTrue(
            names,
            "no FleetSandbox CR for this workspace after fixture+tick; "
            "fleet-host must lease the claim and create the CR",
        )

        st, _, raw = _json_request(opener, "POST", f"{fleet}/runs/{run_id}/cancel")
        self.assertEqual(st, 200, f"cancel {st}: {raw[:400]}")
        deadline = time.monotonic() + 120
        leftover = names
        leftover_pods: list[str] = []
        leftover_nps: list[str] = []
        while time.monotonic() < deadline:
            st, _, raw = _json_request(opener, "POST", f"{fleet}/tick", timeout=60)
            self.assertEqual(st, 200, f"cancel tick {st}: {raw[:400]}")
            leftover = _poll_project_fsb(ws, 8, want_any=False)
            try:
                leftover_pods = _labeled_names("pods", ws)
                leftover_nps = _labeled_names("networkpolicies", ws)
            except RuntimeError:
                time.sleep(1.5)
                continue
            if not leftover and not leftover_pods and not leftover_nps:
                break
        self.assertEqual(
            leftover,
            [],
            f"FleetSandbox still present after cancel+ticks: {leftover}",
        )
        self.assertEqual(
            leftover_pods,
            [],
            f"sandbox pods still present after cancel+ticks: {leftover_pods}",
        )
        self.assertEqual(
            leftover_nps,
            [],
            f"sandbox NetworkPolicies still present after cancel+ticks: {leftover_nps}",
        )
