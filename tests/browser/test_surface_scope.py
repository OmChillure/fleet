#!/usr/bin/env python3
"""surface_map.toml + surface_scope.py stay complete and checkable."""
from __future__ import annotations

import subprocess
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import surface_scope  # noqa: E402
from record_demo import SCENES  # noqa: E402
from test_human_surface_contract import ALIAS_SCENES  # noqa: E402

REPO = Path(__file__).resolve().parents[2]


class SurfaceScopeTests(unittest.TestCase):
    def test_every_scene_has_a_map_rule(self):
        rules = surface_scope.load_rules()
        mapped: set[str] = set()
        for rule in rules:
            mapped.update(rule.get("scenes") or [])
        missing = sorted(set(SCENES) - ALIAS_SCENES - mapped)
        self.assertEqual(
            missing,
            [],
            "surface_map.toml must map every record scene (direct or indirect): "
            + ", ".join(missing),
        )

    def test_map_paths_exist_or_are_prefixes(self):
        missing: list[str] = []
        for rule in surface_scope.load_rules():
            for pref in rule.get("paths") or []:
                p = REPO / pref
                parent = REPO / pref.rsplit("/", 1)[0] if "/" in pref else REPO
                if not p.exists() and not parent.exists():
                    missing.append(pref)
        self.assertEqual(missing, [], f"map paths do not exist: {missing}")

    def test_skip_docs_and_tests(self):
        self.assertTrue(surface_scope.skip_path("docs/human-testing.md"))
        self.assertTrue(surface_scope.skip_path("tests/browser/foo.py"))
        self.assertTrue(surface_scope.skip_path("HUMAN_TEST_SCOPE.md"))
        self.assertFalse(surface_scope.skip_path("crates/fleet-server/src/cases.rs"))

    def test_indirect_db_chat_repo_pulls_chat_and_sessions(self):
        rules = surface_scope.load_rules()
        scenes, pauses, why = surface_scope.hits_for(
            ["crates/fleet-db/src/repos/chats.rs"],
            rules,
        )
        self.assertIn("chat", scenes)
        self.assertIn("sessions", scenes)
        self.assertTrue(any(p[0].endswith("chats.rs") for p in why))
        self.assertIn("Chat history", pauses)
        self.assertIn("This merge request", pauses)

    def test_parse_roundtrip(self):
        text = surface_scope.render(
            "abc",
            "abc-def",
            ["cases", "inbox"],
            ["Cases", "Inbox", "This merge request"],
            [("crates/fleet-server/src/cases.rs", ["cases"])],
        )
        self.assertEqual(
            surface_scope.parse_scenes_from_scope(text),
            ["cases", "inbox"],
        )
        self.assertIn("Film review", text)
        empty = surface_scope.render("abc", "abc", [], ["This merge request"], [])
        self.assertEqual(surface_scope.parse_scenes_from_scope(empty), [])

    def test_scope_assignment_is_literal_in_record_demo(self):
        """Default record path must read HUMAN_TEST_SCOPE.md (not always CORE)."""
        src = (REPO / "tests" / "browser" / "record_demo.py").read_text(encoding="utf-8")
        self.assertIn("parse_scenes_from_scope", src)
        self.assertIn("use_scope", src)

    def test_write_check_agree_on_this_tree(self):
        r = subprocess.run(
            ["python3", "tests/browser/surface_scope.py", "--check"],
            cwd=REPO,
            capture_output=True,
            text=True,
        )
        self.assertEqual(
            r.returncode,
            0,
            r.stderr or r.stdout or "HUMAN_TEST_SCOPE.md stale; run --write",
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
