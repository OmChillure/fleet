#!/usr/bin/env python3
"""Contract: .gitlab-ci.yml creates no pipeline.

This GitLab has no runner. A real job, an include (Auto DevOps), or a
missing file lets GitLab open a pending pipeline that blocks merge.

Comment-stripped body must be only:

  workflow:
    rules:
      - when: never
"""
from __future__ import annotations

import re
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
GITLAB_CI = REPO / ".gitlab-ci.yml"

# Active document only — comments are ignored so docs can mention Auto DevOps.
EXPECTED_ACTIVE = "workflow:\n  rules:\n    - when: never\n"

# GitLab reserved top-level keys that are not jobs, plus job-ish keys that
# would still create a pipeline. Only `workflow` is allowed here.
FORBIDDEN_TOP_LEVEL = (
    "include",
    "stages",
    "variables",
    "default",
    "image",
    "services",
    "cache",
    "before_script",
    "after_script",
    "pages",
    "spec",
)

# Keywords that define a job (or pull one in) if they appear as a mapping key.
FORBIDDEN_KEY_RE = re.compile(
    r"(?m)^[ \t]*(include|script|image|stages|pages|trigger|extends|rules)"
    r"[ \t]*:",
)


def _active_text(text: str) -> str:
    lines: list[str] = []
    for raw in text.replace("\r\n", "\n").replace("\r", "\n").split("\n"):
        stripped = raw.split("#", 1)[0].rstrip()
        if stripped.strip():
            lines.append(stripped)
    return ("\n".join(lines) + "\n") if lines else ""


def _top_level_keys(active: str) -> list[str]:
    keys: list[str] = []
    for line in active.splitlines():
        if not line or line[0] in " \t-":
            continue
        if ":" not in line:
            continue
        keys.append(line.split(":", 1)[0].strip())
    return keys


class GitlabCiWhenNeverTests(unittest.TestCase):
    def test_gitlab_ci_yml_exists(self):
        self.assertTrue(
            GITLAB_CI.is_file(),
            "missing .gitlab-ci.yml lets Auto DevOps create a pending pipeline",
        )

    def test_only_workflow_rules_when_never(self):
        text = GITLAB_CI.read_text(encoding="utf-8")
        active = _active_text(text)
        self.assertEqual(
            active,
            EXPECTED_ACTIVE,
            ".gitlab-ci.yml must be only workflow.rules when: never "
            "(comments stripped); a real job blocks merge",
        )
        self.assertEqual(_top_level_keys(active), ["workflow"])
        self.assertIn("when: never", active)

    def test_no_job_keys_or_auto_devops(self):
        text = GITLAB_CI.read_text(encoding="utf-8")
        active = _active_text(text)
        keys = _top_level_keys(active)
        self.assertEqual(keys, ["workflow"], f"unexpected top-level keys: {keys}")
        for key in FORBIDDEN_TOP_LEVEL:
            self.assertNotIn(
                key,
                keys,
                f"top-level {key!r} is a job or pipeline config; do not add it",
            )
        for line in active.splitlines():
            if line.startswith(" ") or line.startswith("\t"):
                continue
            self.assertFalse(
                FORBIDDEN_KEY_RE.match(line),
                f"job / Auto DevOps key at column 0: {line!r}",
            )
        lowered = active.lower()
        self.assertNotIn("auto-devops", lowered)
        self.assertNotIn("auto_devops", lowered)
        self.assertNotIn("include:", lowered)
        self.assertNotIn("script:", lowered)
        self.assertNotRegex(
            active,
            r"(?m)^(test:|build:|lint:|deploy:|pages:)",
            "a named job key creates a pending pipeline",
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
