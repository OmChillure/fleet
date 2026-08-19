#!/usr/bin/env python3
"""Contract: human walkthrough, record_demo SCENES, and docs stay in sync.

Fails closed when a scene is added/removed in only one of:
  - tests/browser/record_demo.py (SCENES / --list)
  - docs/human-testing.md
  - scripts/human-test.sh (pause_step titles)

Does not start the stack or film video. Run from repo root:

  python3 -m unittest tests.browser.test_record_demo_contract -v
  python3 tests/browser/test_record_demo_contract.py -v
"""
from __future__ import annotations

import ast
import re
import subprocess
import sys
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
REC = REPO / "tests" / "browser" / "record_demo.py"
HUMAN_DOC = REPO / "docs" / "human-testing.md"
HUMAN_SH = REPO / "scripts" / "human-test.sh"

# Canonical scene set (core + lab/kind extras). Order matches record_demo CORE then extras.
EXPECTED_SCENES = (
    "landing",
    "auth",
    "shell",
    "workspace",
    "api",
    "chat",
    "fleet",
    "sandboxes",
    "agents",
    "integrations",
    "settings",
    "workers",
    "sessions",
    "dashboard",
    "lab",
    "kind",
)

# Human walkthrough pause titles (scripts/human-test.sh cmd_guide).
EXPECTED_PAUSE_TITLES = (
    "Landing",
    "Signup",
    "Login",
    "Login wall",
    "Sidebar",
    "Workspace switcher",
    "Settings",
    "Workers",
    "Agents",
    "Integrations",
    "Chat",
    "Fleet",
    "Sandboxes",
    "Chat history",
    "User menu",
    "This merge request",
)


def _list_output() -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(REC), "--list"],
        cwd=str(REPO),
        capture_output=True,
        text=True,
        timeout=30,
    )


def _scenes_from_list_stdout(stdout: str) -> list[str]:
    """Parse scene names printed as indented lines under --list."""
    names: list[str] = []
    for line in stdout.splitlines():
        m = re.match(r"^\s{2}([a-z][a-z0-9_]*)\s*$", line)
        if m:
            names.append(m.group(1))
    return names


def _scenes_dict_keys_from_source(src: str) -> list[str]:
    """Extract string keys of the top-level SCENES dict assignment."""
    tree = ast.parse(src)
    value: ast.expr | None = None
    for node in tree.body:
        if isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
            if node.target.id == "SCENES" and node.value is not None:
                value = node.value
                break
        if isinstance(node, ast.Assign):
            for target in node.targets:
                if isinstance(target, ast.Name) and target.id == "SCENES":
                    value = node.value
                    break
            if value is not None:
                break
    if value is None:
        raise AssertionError("SCENES assignment not found in record_demo.py")
    if not isinstance(value, ast.Dict):
        raise AssertionError("SCENES is not a dict literal")
    keys: list[str] = []
    for k in value.keys:
        if isinstance(k, ast.Constant) and isinstance(k.value, str):
            keys.append(k.value)
        else:
            raise AssertionError(f"non-string SCENES key: {ast.dump(k)}")
    return keys


class RecordDemoContractTests(unittest.TestCase):
    def test_recorder_and_docs_exist(self):
        self.assertTrue(REC.is_file(), REC)
        self.assertTrue(HUMAN_DOC.is_file(), HUMAN_DOC)
        self.assertTrue(HUMAN_SH.is_file(), HUMAN_SH)
        self.assertTrue(HUMAN_SH.stat().st_mode & 0o111, "human-test.sh must be executable")

    def test_list_exits_zero_and_prints_every_scene(self):
        r = _list_output()
        self.assertEqual(r.returncode, 0, r.stderr or r.stdout)
        out = r.stdout
        for scene in EXPECTED_SCENES:
            self.assertRegex(
                out,
                rf"(?m)^\s{{2}}{re.escape(scene)}\s*$",
                f"--list must print scene {scene!r}",
            )
        listed = _scenes_from_list_stdout(out)
        self.assertEqual(
            set(listed),
            set(EXPECTED_SCENES),
            f"--list scenes {listed} != expected {list(EXPECTED_SCENES)}",
        )

    def test_scenes_dict_keys_match_list(self):
        src = REC.read_text(encoding="utf-8")
        keys = _scenes_dict_keys_from_source(src)
        self.assertEqual(
            set(keys),
            set(EXPECTED_SCENES),
            f"SCENES keys {keys} != expected {list(EXPECTED_SCENES)}",
        )
        r = _list_output()
        self.assertEqual(r.returncode, 0, r.stderr or r.stdout)
        listed = _scenes_from_list_stdout(r.stdout)
        self.assertEqual(
            set(keys),
            set(listed),
            f"SCENES keys {keys} != --list {listed}",
        )

    def test_human_testing_doc_mentions_each_scene(self):
        body = HUMAN_DOC.read_text(encoding="utf-8")
        for scene in EXPECTED_SCENES:
            # Backticks (`landing`) or markdown table cells (| `landing` |).
            self.assertRegex(
                body,
                rf"`{re.escape(scene)}`|`{re.escape(scene)}\s",
                f"docs/human-testing.md must mention scene `{scene}`",
            )

    def test_human_test_sh_pause_steps(self):
        text = HUMAN_SH.read_text(encoding="utf-8")
        self.assertIn("pause_step()", text)
        for title in EXPECTED_PAUSE_TITLES:
            self.assertIn(
                f'pause_step "{title}',
                text,
                f'scripts/human-test.sh must pause_step "{title}…"',
            )

    def test_require_stack_uses_this_tree_identity(self):
        src = REC.read_text(encoding="utf-8")
        self.assertIn("def require_stack", src)
        self.assertIn("require_this_tree", src)
        self.assertIn("ForeignStackError", src)
        ident = (REPO / "tests" / "browser" / "stack_identity.py").read_text(encoding="utf-8")
        self.assertIn("FLEET_GIT_SHA", ident)
        self.assertIn("git_sha", ident)
        self.assertIn("fleet-git-sha.txt", ident)
        self.assertIn("extraPortMappings", ident)
        self.assertIn('"chat"', ident)
        self.assertNotIn("FLEET_ALLOW_FOREIGN_STACK", ident)
        self.assertNotIn("continuing", ident.lower())
        # require_stack must gate recording (called before launch), not only defined.
        self.assertRegex(
            src,
            r"require_stack\(\)\s*\n",
            "main path must call require_stack() so down ports fail closed",
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
