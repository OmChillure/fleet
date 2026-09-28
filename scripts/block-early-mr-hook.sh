#!/usr/bin/env python3
"""PreToolUse hook: deny commands that create a GitLab MR / GitHub PR.

Opening an MR is the green light to merge. The only allowed creator is a
script named open-mr.sh (repo scripts/open-mr.sh or the user skill copy).
Fail-open on unreadable stdin so a broken payload cannot wedge the session.
"""
from __future__ import annotations

import json
import re
import sys

CREATE_RE = re.compile(
    r"""
    merge_request\.create
    | \bglab\s+mr\s+create\b
    | \bgitlab\s+mr\s+create\b
    | \bgh\s+pr\s+create\b
    | api/v4/projects/.*/merge_requests
    | /merge_requests(?:\s|'|"|$|\?)
    """,
    re.I | re.X,
)

DENY_REASON = (
    "Opening an MR is the green light to merge. This command would create "
    "an MR before the merge bar is proven. Run scripts/open-mr.sh only after "
    "scripts/assert-mr-ready.sh passes (just test, just test-integration, "
    "ship images on this tree-id, film unless small tweak). Never "
    "git push -o merge_request.create, glab mr create, or gh pr create."
)


def allow() -> None:
    sys.stdout.write('{"decision":"allow"}\n')
    sys.exit(0)


def deny(reason: str) -> None:
    sys.stdout.write(json.dumps({"decision": "deny", "reason": reason}) + "\n")
    sys.exit(2)


def command_from_event(ev: object) -> str:
    if not isinstance(ev, dict):
        return ""
    ti = ev.get("toolInput")
    if ti is None:
        ti = ev.get("tool_input")
    if isinstance(ti, dict):
        cmd = ti.get("command")
        return cmd if isinstance(cmd, str) else ""
    if isinstance(ti, str):
        return ti
    return ""


def decide(cmd: str) -> str:
    """Return 'allow' or a deny reason.

    `open-mr.sh` alone does not match CREATE_RE. Naming that script in the
    same command as `glab mr create`, `gh pr create`, or a merge-request
    POST does not make the create legal.
    """
    if CREATE_RE.search(cmd):
        return DENY_REASON
    return "allow"


def self_test() -> int:
    cases = [
        ("git status", "allow"),
        ("git push gitlab HEAD", "allow"),
        ("git push gitlab HEAD -o merge_request.create", "deny"),
        (
            "git push gitlab HEAD -o merge_request.create -o merge_request.target=main",
            "deny",
        ),
        ("scripts/open-mr.sh", "allow"),
        ("./scripts/open-mr.sh --title x", "allow"),
        ("bash /home/viny/.grok/skills/open-mr/scripts/open-mr.sh", "allow"),
        ("glab mr create --title x", "deny"),
        ("gh pr create --fill", "deny"),
        (
            "curl -X POST https://nova.teachx.ai/api/v4/projects/1/merge_requests",
            "deny",
        ),
        (
            "scripts/open-mr.sh && git push gitlab HEAD -o merge_request.create",
            "deny",
        ),
        ("scripts/open-mr.sh && glab mr create --title x", "deny"),
        ("scripts/open-mr.sh && gh pr create --fill", "deny"),
        (
            "scripts/open-mr.sh; curl -X POST https://nova.teachx.ai/api/v4/projects/1/merge_requests",
            "deny",
        ),
    ]
    failed = 0
    for cmd, want in cases:
        got = decide(cmd)
        ok = (got == "allow") if want == "allow" else (got != "allow")
        if not ok:
            print(f"FAIL: {cmd!r} wanted {want}, got {got[:40]!r}", file=sys.stderr)
            failed += 1
    if failed:
        print(f"{failed} hook self-test(s) failed", file=sys.stderr)
        return 1
    print("block-early-mr-hook self-test ok")
    return 0


def main(argv: list[str]) -> int:
    if argv[1:] == ["--self-test"]:
        return self_test()
    raw = sys.stdin.read()
    if not raw.strip():
        allow()
    try:
        ev = json.loads(raw)
    except json.JSONDecodeError:
        allow()
    verdict = decide(command_from_event(ev))
    if verdict == "allow":
        allow()
    deny(verdict)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
