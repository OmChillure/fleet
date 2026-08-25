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


class KindSmokeContractTests(unittest.TestCase):
    def test_detection_is_not_host_http_health(self):
        import inspect

        src = inspect.getsource(_cluster)
        self.assertIn("agent-fleet", src)
        self.assertIn("kubectl", src)
        self.assertNotIn("127.0.0.1:8080", src)
        self.assertNotIn("/health", src)

    def test_kind_smoke_covers_in_cluster_k8s_path(self):
        src = Path(__file__).read_text(encoding="utf-8")
        self.assertIn("SANDBOX_BACKEND", src)
        self.assertIn("fleet-api", src)
        self.assertIn("runs/fixture", src)
        self.assertIn("fleet.agentfleet.io/project-id", src)
        self.assertIn("networkpolicies", src)
        self.assertIn("fsb", src)
        self.assertIn("fleet-driver", src)
        self.assertIn("FLEET_HARNESS", src)
        self.assertIn("zenohd", src)
        self.assertIn("/usr/local/bin", src)


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

    def test_control_plane_deployments_ready(self):
        for name in ("api", "ui", "postgres", "fleet-operator", "fleet-host", "zenohd"):
            r = _kubectl(
                "-n",
                "agent-fleet",
                "get",
                "deploy",
                name,
                "-o",
                "jsonpath={.status.readyReplicas}",
            )
            self.assertEqual(r.returncode, 0, f"kubectl get deploy {name}: {r.stderr.strip()[:200]}")
            self.assertGreaterEqual(int(r.stdout or "0"), 1, f"{name} readyReplicas={r.stdout!r}")

    def test_crd_installed(self):
        r = _kubectl("get", "crd", "fleetsandboxes.fleet.agentfleet.io", "-o", "name")
        self.assertEqual(r.returncode, 0, f"kubectl get crd: {r.stderr.strip()[:200]}")
        self.assertIn("fleetsandboxes", r.stdout)

    def test_host_api_health(self):
        try:
            require_this_tree(_REPO, "http://127.0.0.1:8080", require_ui=True)
        except ForeignStackError as e:
            self.fail(
                f"host :8080/:3030 is not this tree: {e}. "
                "Rebuild agent-fleet-api:local and agent-fleet-ui:local "
                "(install script or ./scripts/run-every-test.sh --prepare-kind). "
                "Do not kubectl set env FLEET_GIT_SHA. SKIP_KIND=1 only if there is no cluster."
            )

    def test_host_ui(self):
        try:
            require_this_tree(_REPO, "http://127.0.0.1:8080", require_ui=True)
        except ForeignStackError as e:
            self.fail(f"host :3030 is not this tree: {e}")
        try:
            with urllib.request.urlopen("http://127.0.0.1:3030/", timeout=5) as resp:
                self.assertEqual(resp.status, 200)
            with urllib.request.urlopen(
                "http://127.0.0.1:3030/fleet-git-sha.txt", timeout=5
            ) as resp:
                self.assertEqual(resp.status, 200)
                sha = resp.read().decode().strip()
        except (urllib.error.URLError, TimeoutError) as e:
            self.fail(f"host :3030: {e}")
        want = subprocess.check_output(
            ["bash", str(_REPO / "scripts" / "tree-id.sh")],
            text=True,
            cwd=str(_REPO),
        ).strip()
        self.assertEqual(sha, want, f"UI /fleet-git-sha.txt={sha!r} != tree-id {want}")

    def test_api_is_not_a_runner_and_host_owns_crs(self):
        backend = _deploy_env("SANDBOX_BACKEND")
        self.assertEqual(
            backend,
            "stub",
            f"api deploy SANDBOX_BACKEND={backend!r}; apply infra/overlays/kind",
        )
        host = _kubectl(
            "-n",
            "agent-fleet",
            "get",
            "deploy",
            "fleet-host",
            "-o",
            "jsonpath={.spec.template.spec.serviceAccountName}",
        )
        self.assertEqual(host.returncode, 0, host.stderr)
        self.assertEqual((host.stdout or "").strip(), "fleet-host")
        api_role = _kubectl("-n", "agent-fleet", "get", "role", "fleet-api", "-o", "yaml")
        self.assertEqual(api_role.returncode, 0, api_role.stderr)
        self.assertNotIn("fleetsandboxes", api_role.stdout)
        host_role = _kubectl("-n", "agent-fleet", "get", "role", "fleet-host", "-o", "yaml")
        self.assertEqual(host_role.returncode, 0, host_role.stderr)
        self.assertIn("fleetsandboxes", host_role.stdout)

    def test_api_image_has_driver_not_worker(self):
        try:
            require_this_tree(_REPO, "http://127.0.0.1:8080", require_ui=True)
        except ForeignStackError as e:
            self.fail(f"host :8080/:3030 is not this tree: {e}")
        ls = _kubectl(
            "-n",
            "agent-fleet",
            "exec",
            "deploy/api",
            "--",
            "ls",
            "/usr/local/bin",
        )
        self.assertEqual(ls.returncode, 0, ls.stderr)
        bins = ls.stdout or ""
        self.assertIn("fleet-driver", bins)
        self.assertIn("mock-acp-agent", bins)
        self.assertNotIn("fleet-worker", bins)

    def test_tick_creates_and_cancel_deletes_fleetsandbox(self):
        try:
            require_this_tree(_REPO, "http://127.0.0.1:8080", require_ui=True)
        except ForeignStackError as e:
            self.fail(
                f"host :8080/:3030 is not this tree: {e}. "
                "Rebuild + apply before treating kind smoke as valid."
            )
        self.assertEqual(_deploy_env("SANDBOX_BACKEND"), "stub")

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
        st, proj, raw = _json_request(opener, "GET", fleet)
        self.assertEqual(st, 200, f"GET fleet {st}: {raw[:400]}")
        self.assertIsInstance(proj, dict)
        self.assertEqual(
            (proj or {}).get("sandbox_backend"),
            "stub",
            f"runtime backend: {proj}",
        )
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
        cr = _kubectl(
            "-n",
            "agent-fleet",
            "get",
            "fsb",
            names[0],
            "-o",
            "json",
        )
        self.assertEqual(cr.returncode, 0, cr.stderr)
        spec = (json.loads(cr.stdout or "{}") or {}).get("spec") or {}
        self.assertEqual(
            spec.get("command"),
            ["fleet-driver"],
            f"guest command must be fleet-driver, got {spec.get('command')!r}",
        )
        cr_env = spec.get("env") or {}
        self.assertEqual(
            cr_env.get("FLEET_HARNESS"),
            "mock",
            f"guest FLEET_HARNESS={cr_env.get('FLEET_HARNESS')!r}",
        )
        pod_names: list[str] = []
        deadline = time.monotonic() + 45
        while time.monotonic() < deadline:
            pods = _kubectl(
                "-n",
                "agent-fleet",
                "get",
                "pods",
                "-l",
                f"{_LABEL_PROJECT}={ws}",
                "-o",
                "jsonpath={.items[*].metadata.name}",
            )
            self.assertEqual(pods.returncode, 0, pods.stderr)
            pod_names = [n for n in (pods.stdout or "").split() if n]
            if pod_names:
                break
            time.sleep(1.5)
        self.assertTrue(
            pod_names,
            f"operator did not create a sandbox pod for workspace {ws}",
        )
        pod_cmd = _kubectl(
            "-n",
            "agent-fleet",
            "get",
            "pod",
            pod_names[0],
            "-o",
            "jsonpath={.spec.containers[0].command}",
        )
        self.assertEqual(pod_cmd.returncode, 0, pod_cmd.stderr)
        self.assertIn(
            "fleet-driver",
            pod_cmd.stdout or "",
            f"sandbox pod command={pod_cmd.stdout!r}",
        )
        nps = _kubectl(
            "-n",
            "agent-fleet",
            "get",
            "networkpolicies",
            "-l",
            f"{_LABEL_PROJECT}={ws}",
            "-o",
            "jsonpath={.items[*].metadata.name}",
        )
        self.assertEqual(nps.returncode, 0, nps.stderr)
        self.assertTrue(
            (nps.stdout or "").split(),
            "operator did not create a sandbox NetworkPolicy",
        )

        st, _, raw = _json_request(opener, "POST", f"{fleet}/runs/{run_id}/cancel")
        self.assertEqual(st, 200, f"cancel {st}: {raw[:400]}")
        deadline = time.monotonic() + 120
        leftover = names
        while time.monotonic() < deadline:
            st, _, raw = _json_request(opener, "POST", f"{fleet}/tick", timeout=60)
            self.assertEqual(st, 200, f"cancel tick {st}: {raw[:400]}")
            leftover = _poll_project_fsb(ws, 8, want_any=False)
            if not leftover:
                break
        self.assertEqual(
            leftover,
            [],
            f"FleetSandbox still present after cancel+ticks: {leftover}",
        )

    def test_second_workspace_cluster_host_still_creates_fsb(self):
        try:
            require_this_tree(_REPO, "http://127.0.0.1:8080", require_ui=True)
        except ForeignStackError as e:
            self.fail(f"host :8080/:3030 is not this tree: {e}")
        jar = http.cookiejar.CookieJar()
        opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
        email = f"kind-ws2-{uuid.uuid4().hex[:12]}@example.test"
        st, body, raw = _json_request(
            opener,
            "POST",
            "http://127.0.0.1:8080/api/auth/signup",
            {
                "email": email,
                "password": "kind-pass-ok",
                "display_name": "kind-ws2",
            },
        )
        self.assertIn(st, {200, 201}, f"signup {st}: {raw[:400]}")
        st, body, raw = _json_request(
            opener,
            "POST",
            "http://127.0.0.1:8080/api/auth/login",
            {"email": email, "password": "kind-pass-ok"},
        )
        self.assertIn(st, {200, 201}, f"login {st}: {raw[:400]}")
        ws = str(((body or {}).get("default_workspace") or {}).get("id") or "")
        self.assertTrue(ws)
        fleet = f"http://127.0.0.1:8080/api/workspaces/{ws}/fleet"
        st, _, raw = _json_request(opener, "GET", fleet)
        self.assertEqual(st, 200, raw[:400])
        deadline = time.monotonic() + 90
        host_online = False
        while time.monotonic() < deadline:
            st, hosts, raw = _json_request(
                opener, "GET", f"http://127.0.0.1:8080/api/workspaces/{ws}/worker-hosts"
            )
            self.assertEqual(st, 200, raw[:400])
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
        self.assertTrue(host_online, "cluster host must appear on a second workspace")
        st, fx, raw = _json_request(
            opener, "POST", f"{fleet}/runs/fixture", {"kind": "chain"}
        )
        self.assertEqual(st, 201, raw[:400])
        st, _, raw = _json_request(opener, "POST", f"{fleet}/tick", timeout=60)
        self.assertEqual(st, 200, raw[:400])
        names = _poll_project_fsb(ws, 90, want_any=True)
        self.assertTrue(names, "second workspace must get a FleetSandbox")
