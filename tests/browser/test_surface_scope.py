#!/usr/bin/env python3
"""surface_map.toml + surface_scope.py stay complete and checkable."""
from __future__ import annotations

import io
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import record_demo  # noqa: E402
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

    def test_map_paths_exist_or_path_match_a_tracked_path(self):
        tracked = subprocess.run(
            ["git", "ls-files"],
            cwd=REPO,
            check=True,
            capture_output=True,
            text=True,
        ).stdout.splitlines()
        missing: list[str] = []
        for rule in surface_scope.load_rules():
            for pref in rule.get("paths") or []:
                if (REPO / pref).exists():
                    continue
                if any(surface_scope.path_matches(path, pref) for path in tracked):
                    continue
                missing.append(pref)
        self.assertEqual(
            missing,
            [],
            "surface_map.toml prefixes must exist or path_match a tracked path: "
            + ", ".join(missing),
        )

    def test_path_matches_slash_boundary_not_string_prefix(self):
        self.assertFalse(
            surface_scope.path_matches(
                "crates/fleet-ui/src/pages/home.rs",
                "crates/fleet-ui/src/pages/home",
            )
        )
        self.assertTrue(
            surface_scope.path_matches(
                "crates/fleet-ui/src/pages/home/page.rs",
                "crates/fleet-ui/src/pages/home",
            )
        )

    def test_skip_docs_and_tests(self):
        # Skip must win even when a map rule would film these paths.
        bait = [
            {
                "paths": ["docs/", "tests/", "HUMAN_TEST_SCOPE.md"],
                "scenes": ["cases"],
                "pauses": ["Cases"],
            }
        ]
        scenes, _, why = surface_scope.hits_for(
            [
                "docs/human-testing.md",
                "tests/browser/foo.py",
                "HUMAN_TEST_SCOPE.md",
            ],
            bait,
        )
        self.assertEqual(scenes, [])
        self.assertEqual(why, [])

    def test_skip_path_other_non_product_roots(self):
        skipped = [
            "AGENTS.md",
            "justfile",
            "Cargo.toml",
            "Cargo.lock",
            "README.md",
            "development.md",
            "docker-compose.yml",
            ".gitlab-ci.yml",
        ]
        bait = [{"paths": list(skipped), "scenes": ["cases"], "pauses": ["Cases"]}]
        scenes, _, why = surface_scope.hits_for(skipped, bait)
        self.assertEqual(scenes, [])
        self.assertEqual(why, [])

    def test_skip_path_does_not_hide_mapped_operator_paths(self):
        paths = [
            "scripts/install-worker",
            "scripts/stage-worker-dist.sh",
            "Dockerfile",
            "runner-packages.toml",
            "infra/kind.yaml",
        ]
        scenes, _, why = surface_scope.hits_for(paths, surface_scope.load_rules())
        self.assertIn("runners", scenes)
        self.assertIn("kind", scenes)
        why_paths = {hit[0] for hit in why}
        self.assertEqual(why_paths, set(paths))

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
        """main([]) films HUMAN_TEST_SCOPE.md scenes, not CORE."""
        want = ["chat", "cases"]
        self.assertNotEqual(want, record_demo.CORE)
        text = surface_scope.render(
            "abc",
            "abc-def",
            want,
            ["Chat", "Cases", "This merge request"],
            [],
        )
        with tempfile.TemporaryDirectory() as td:
            scope = Path(td) / "HUMAN_TEST_SCOPE.md"
            scope.write_text(text, encoding="utf-8")
            buf = io.StringIO()
            with patch.object(surface_scope, "SCOPE", scope):
                with patch.object(
                    record_demo,
                    "require_stack",
                    side_effect=SystemExit("stop-before-film"),
                ) as require_stack:
                    with redirect_stdout(buf):
                        with self.assertRaises(SystemExit) as raised:
                            record_demo.main([])
                    self.assertEqual(str(raised.exception), "stop-before-film")
                    require_stack.assert_called_once()
        resolved: list[str] | None = None
        for line in buf.getvalue().splitlines():
            if line.startswith("from-scope:"):
                rest = line.split(":", 1)[1].strip()
                resolved = rest.split() if rest else []
                break
        self.assertEqual(resolved, want)
        self.assertNotEqual(resolved, record_demo.CORE)

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
