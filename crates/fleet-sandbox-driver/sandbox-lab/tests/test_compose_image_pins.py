#!/usr/bin/env python3
"""Compose workspace/driver use stable image tags (reuse across fleet-task-*)."""
from __future__ import annotations

import unittest
from pathlib import Path

LAB = Path(__file__).resolve().parents[1]
COMPOSE = LAB / "docker-compose.yml"
INSTALL = LAB.parents[2] / "scripts" / "install-full-suite-deps.sh"
WS_TAG = "agent-fleet-sandbox-workspace:local"
DR_TAG = "agent-fleet-sandbox-driver:local"
PINNED = (
    ("workspace", "COMPOSE_SANDBOX_WORKSPACE_IMAGE", WS_TAG),
    ("driver", "COMPOSE_SANDBOX_DRIVER_IMAGE", DR_TAG),
)


def _service_image_and_build(text: str, name: str) -> tuple[str | None, bool]:
    """Top-level image/build on services.<name> (ignores comments and siblings)."""
    header = f"  {name}:"
    lines = text.splitlines()
    start = None
    for i, line in enumerate(lines):
        if line.split("#", 1)[0].rstrip() == header:
            start = i + 1
            break
    if start is None:
        return None, False
    image = None
    has_build = False
    for line in lines[start:]:
        body = line.split("#", 1)[0].rstrip()
        if not body.strip():
            continue
        indent = len(body) - len(body.lstrip(" "))
        if indent <= 2:
            break
        if indent != 4:
            continue
        key, sep, rest = body.strip().partition(":")
        if not sep:
            continue
        if key == "image":
            image = rest.strip().strip("\"'")
        elif key == "build":
            has_build = True
    return image, has_build


def _sandbox_lab_cmds_after_cd_lab(text: str) -> list[str]:
    """ENABLE_SANDBOX_LAB=1 cmds after cd \"${LAB}\" (comments stripped, until next cd)."""
    header = 'if [[ "${ENABLE_SANDBOX_LAB}" == "1" ]]; then'
    lines = text.splitlines()
    start = None
    for i, line in enumerate(lines):
        if line.split("#", 1)[0].rstrip() == header:
            start = i + 1
            break
    if start is None:
        return []
    header_indent = len(lines[start - 1]) - len(lines[start - 1].lstrip(" "))
    cmds: list[str] = []
    for line in lines[start:]:
        body = line.split("#", 1)[0].rstrip()
        if not body.strip():
            continue
        indent = len(body) - len(body.lstrip(" "))
        if indent <= header_indent and body.strip() == "fi":
            break
        cmds.append(body.strip())
    try:
        lab_i = cmds.index('cd "${LAB}"')
    except ValueError:
        return []
    after = cmds[lab_i + 1 :]
    stop = next((i for i, c in enumerate(after) if c.startswith("cd ")), len(after))
    return after[:stop]


class ComposeImagePinTests(unittest.TestCase):
    def test_compose_declares_stable_image_and_keeps_build(self):
        text = COMPOSE.read_text(encoding="utf-8")
        for name, env_name, tag in PINNED:
            image, has_build = _service_image_and_build(text, name)
            self.assertEqual(image, f"${{{env_name}:-{tag}}}", name)
            self.assertTrue(has_build, f"{name} must keep build:")

    def test_install_script_builds_pinned_names(self):
        text = INSTALL.read_text(encoding="utf-8")
        window = _sandbox_lab_cmds_after_cd_lab(text)
        self.assertTrue(window, 'ENABLE_SANDBOX_LAB=1 must cd "${LAB}" then dc build')
        self.assertIn("dc build workspace", window)
        self.assertIn("dc build --pull=false driver", window)
        self.assertLess(
            window.index("dc build workspace"),
            window.index("dc build --pull=false driver"),
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
