#!/usr/bin/env python3
"""Throwaway container that acts as a paired fleet-host with in-box Ollama.

Image `fleet-live-sim-host:local` is built once (docker.io + ollama bins).
Model weights come from the host `~/.ollama` mount so pulls persist.
Does not rotate the laptop's machine token.
"""
from __future__ import annotations

import hashlib
import os
import shutil
import subprocess
import tempfile
import time
from pathlib import Path

LIVE_URL = os.environ.get("FLEET_LIVE_URL", "https://agentfleet.studio").rstrip("/")
BASE_IMAGE = os.environ.get(
    "FLEET_LIVE_HOST_BASE",
    "agent-fleet-api:ace6eae53e2cba80516f1171a83084488eeadec6",
)
SIM_IMAGE = os.environ.get("FLEET_LIVE_HOST_IMAGE", "fleet-live-sim-host:local")
DOCKER_IMAGE = os.environ.get("FLEET_LIVE_JOB_IMAGE", "agent-fleet-api:local")
OLLAMA_MODEL = os.environ.get("FLEET_LIVE_OLLAMA_MODEL", "gemma3:270m")
OLLAMA_PORT = os.environ.get("FLEET_LIVE_OLLAMA_PORT", "11435")
REPO = Path(__file__).resolve().parents[2]
DOCKERFILE = Path(__file__).resolve().parent / "Dockerfile.live-sim-host"


def _run(cmd: list[str], **kwargs) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, check=True, text=True, **kwargs)


def _image_exists(tag: str) -> bool:
    out = subprocess.run(
        ["docker", "image", "inspect", tag],
        capture_output=True,
        text=True,
    )
    return out.returncode == 0


def _dockerfile_stamp() -> str:
    raw = DOCKERFILE.read_bytes() + BASE_IMAGE.encode()
    return hashlib.sha256(raw).hexdigest()[:16]


def ensure_sim_image() -> str:
    """Build `fleet-live-sim-host:local` only when the Dockerfile/base changed."""
    stamp = _dockerfile_stamp()
    if _image_exists(SIM_IMAGE):
        labels = subprocess.run(
            [
                "docker",
                "image",
                "inspect",
                "-f",
                "{{index .Config.Labels \"fleet.live-sim-stamp\"}}",
                SIM_IMAGE,
            ],
            capture_output=True,
            text=True,
        )
        if labels.stdout.strip() == stamp:
            return SIM_IMAGE
    _run(
        [
            "docker",
            "build",
            "-t",
            SIM_IMAGE,
            "--build-arg",
            f"BASE={BASE_IMAGE}",
            "--label",
            f"fleet.live-sim-stamp={stamp}",
            "-f",
            str(DOCKERFILE),
            str(DOCKERFILE.parent),
        ]
    )
    return SIM_IMAGE


class ThrowawayHost:
    def __init__(self, stamp: str) -> None:
        self.name = f"live-sim-host-{stamp}"
        self.root = Path(tempfile.mkdtemp(prefix=f"{self.name}-"))
        self.cfg_dir = self.root / "cfg"
        self.work_dir = self.root / "work"
        self.cfg_dir.mkdir(parents=True)
        self.work_dir.mkdir(parents=True)
        self.started = False
        self.ollama_base = f"http://127.0.0.1:{OLLAMA_PORT}"
        self.model = OLLAMA_MODEL

    def start(self) -> None:
        ensure_sim_image()
        kube = Path.home() / ".kube" / "config"
        ollama_home = Path.home() / ".ollama"
        volumes = [
            "-v",
            "/var/run/docker.sock:/var/run/docker.sock",
            "-v",
            f"{self.root}:{self.root}",
        ]
        if ollama_home.is_dir():
            volumes += ["-v", f"{ollama_home}:/root/.ollama"]
        if kube.is_file():
            volumes += ["-v", f"{kube}:/root/.kube/config:ro"]
        kubectl = Path("/usr/local/bin/kubectl")
        if kubectl.is_file():
            volumes += ["-v", f"{kubectl}:/usr/local/bin/kubectl:ro"]
        _run(
            [
                "docker",
                "run",
                "-d",
                "--name",
                self.name,
                "--hostname",
                self.name,
                "--network",
                "host",
                "--user",
                "0:0",
                *volumes,
                "-e",
                f"FLEET_HOST_CONFIG_DIR={self.cfg_dir}",
                "-e",
                "FLEET_HOST_RUN_HOST=true",
                "-e",
                "FLEET_HOST_RUN_CONTAINER=true",
                "-e",
                "HOME=/root",
                "-e",
                f"OLLAMA_HOST=127.0.0.1:{OLLAMA_PORT}",
                "-e",
                f"OLLAMA_BASE_URL={self.ollama_base}",
                "--entrypoint",
                "sleep",
                SIM_IMAGE,
                "infinity",
            ],
            capture_output=True,
        )
        self.started = True
        self.write_config(backend="docker")
        self._start_ollama()

    def _start_ollama(self) -> None:
        _run(
            [
                "docker",
                "exec",
                "-d",
                self.name,
                "sh",
                "-c",
                f"OLLAMA_HOST=127.0.0.1:{OLLAMA_PORT} ollama serve "
                f"> /tmp/ollama.log 2>&1",
            ],
            capture_output=True,
        )
        deadline = time.time() + 60
        last = ""
        while time.time() < deadline:
            probe = subprocess.run(
                [
                    "docker",
                    "exec",
                    self.name,
                    "curl",
                    "-sf",
                    f"{self.ollama_base}/api/tags",
                ],
                capture_output=True,
                text=True,
            )
            if probe.returncode == 0:
                show = subprocess.run(
                    ["docker", "exec", self.name, "ollama", "show", self.model],
                    capture_output=True,
                    text=True,
                )
                if show.returncode != 0:
                    pull = subprocess.run(
                        ["docker", "exec", self.name, "ollama", "pull", self.model],
                        capture_output=True,
                        text=True,
                    )
                    if pull.returncode != 0:
                        last = pull.stderr or pull.stdout
                        time.sleep(1)
                        continue
                warm = subprocess.run(
                    [
                        "docker",
                        "exec",
                        self.name,
                        "ollama",
                        "run",
                        self.model,
                        "hi",
                    ],
                    capture_output=True,
                    text=True,
                    timeout=180,
                )
                if warm.returncode == 0:
                    return
                last = warm.stderr or warm.stdout
            else:
                last = probe.stderr or probe.stdout
            time.sleep(1)
        log = ""
        try:
            log = self.exec(
                ["sh", "-c", "ps aux | grep -i ollama; echo '---'; tail -n 40 /tmp/ollama.log"],
                capture_output=True,
            ).stdout
        except Exception as e:
            log = str(e)
        raise RuntimeError(
            f"ollama in {self.name} not ready: {last[-400:]}\n{log[-800:]}"
        )

    def write_config(self, *, backend: str) -> None:
        path = self.cfg_dir / "config.toml"
        existing = path.read_text() if path.is_file() else ""
        host_id = ""
        url = ""
        for line in existing.splitlines():
            if line.startswith("host_id"):
                host_id = line
            if line.startswith("control_plane_url"):
                url = line
        extras = "\n".join(x for x in (url, host_id) if x)
        path.write_text(
            f"""run_host = true
run_container = true
container_backend = "{backend}"
work_dir = "{self.work_dir}"
auto_update = false
fleet_driver_bin = "fleet-driver"
log_level = "info"
heartbeat_interval_s = 5
poll_interval_s = 2
docker_image = "{DOCKER_IMAGE}"
docker_network = "bridge"
k8s_namespace = "agent-fleet"
allow_privileged = false
{extras}

[extra_env]
FLEET_HARNESS = "ollama"
FLEET_MODEL = "{self.model}"
OLLAMA_BASE_URL = "{self.ollama_base}"
OLLAMA_HTTP_TIMEOUT_SECS = "600"
FLEET_MOCK_ACP_BIN = "/usr/local/bin/mock-acp-agent"
"""
        )

    def exec(self, argv: list[str], **kwargs) -> subprocess.CompletedProcess:
        return _run(["docker", "exec", self.name, *argv], **kwargs)

    def pair(self, token: str) -> str:
        out = self.exec(
            [
                "fleet-host",
                "pair",
                "--url",
                LIVE_URL,
                "--token",
                token,
            ],
            capture_output=True,
        )
        return out.stdout + out.stderr

    def run_detached(self) -> None:
        _run(
            [
                "docker",
                "exec",
                "-d",
                self.name,
                "sh",
                "-c",
                "fleet-host run > /tmp/fleet-host.log 2>&1",
            ],
            capture_output=True,
        )

    def stop_run(self) -> None:
        self.exec(
            [
                "sh",
                "-c",
                "pkill -TERM -f 'fleet-host run' || true; sleep 2; "
                "pkill -KILL -f 'fleet-host run' || true; "
                f"rm -f {self.cfg_dir}/run.lock",
            ],
            capture_output=True,
        )

    def restart_run(self) -> None:
        self.stop_run()
        time.sleep(1)
        self.run_detached()
        time.sleep(2)

    def logs(self) -> str:
        try:
            out = subprocess.run(
                ["docker", "exec", self.name, "tail", "-n", "160", "/tmp/fleet-host.log"],
                capture_output=True,
                text=True,
            )
            return (out.stdout or "") + (out.stderr or "")
        except Exception:
            return ""

    def stop(self) -> None:
        if self.started:
            subprocess.run(
                ["docker", "rm", "-f", self.name],
                check=False,
                capture_output=True,
                text=True,
            )
            self.started = False
        shutil.rmtree(self.root, ignore_errors=True)
