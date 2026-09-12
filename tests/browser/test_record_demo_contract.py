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
import json
import re
import subprocess
import sys
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
REC = REPO / "tests" / "browser" / "record_demo.py"
HUMAN_DOC = REPO / "docs" / "human-testing.md"
HUMAN_SH = REPO / "scripts" / "human-test.sh"

sys.path.insert(0, str(REC.parent))
import surface_scope  # noqa: E402
from test_human_surface_contract import PAUSE_HEAD_RE, scene_keys  # noqa: E402


def _pause_head_titles(text: str) -> list[str]:
    """Exact pause_step quoted titles from PAUSE_HEAD_RE."""
    return PAUSE_HEAD_RE.findall(text)


def _inventory_title(raw: str) -> str:
    # cmd_guide may append "  ${UI_URL}…"; map titles use a single space.
    return re.split(r"  +", raw, maxsplit=1)[0]


def _required_pause_titles() -> set[str]:
    titles = set(surface_scope.ALWAYS_PAUSE)
    for rule in surface_scope.load_rules():
        titles.update(rule.get("pauses") or [])
    return titles


def _crate_scene_table(doc: str) -> str:
    """Filming map under 'Which scene for which code', not extras prose."""
    match = re.search(
        r"(\| You changed \| Scene \| The video must show \|\n(?:\|.*\n)*)",
        doc,
    )
    if not match:
        raise AssertionError(
            "docs/human-testing.md missing crate→scene table "
            "(| You changed | Scene | The video must show |)"
        )
    return match.group(1)


def _serve_health(git_sha: str, apis: list[str]):
    """Local GET /health. Caller must shutdown the returned server."""
    state = {"git_sha": git_sha, "apis": list(apis)}

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path.split("?", 1)[0] != "/health":
                self.send_error(404)
                return
            payload = json.dumps(
                {
                    "status": "ok",
                    "service": "fleet-server",
                    "db": "up",
                    "git_sha": state["git_sha"],
                    "apis": state["apis"],
                }
            ).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def log_message(self, *args):
            return

    httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{httpd.server_address[1]}"
    return httpd, url, state


def _role_name(call: ast.Call) -> str | None:
    if not (
        isinstance(call.func, ast.Attribute) and call.func.attr == "get_by_role"
    ):
        return None
    for kw in call.keywords:
        if kw.arg == "name" and isinstance(kw.value, ast.Constant):
            val = kw.value.value
            if isinstance(val, str):
                return val
    return None


def _click_role_name(call: ast.Call, bound: dict[str, str]) -> str | None:
    if not (isinstance(call.func, ast.Attribute) and call.func.attr == "click"):
        return None
    cur: ast.AST = call.func.value
    while True:
        if isinstance(cur, ast.Call):
            name = _role_name(cur)
            if name:
                return name
            nxt = cur.func.value if isinstance(cur.func, ast.Attribute) else cur.func
            cur = nxt
        elif isinstance(cur, ast.Attribute):
            cur = cur.value
        elif isinstance(cur, ast.Name):
            return bound.get(cur.id)
        else:
            return None


def _clicks_wrapped_in_expect_http(
    fn: ast.FunctionDef, required: set[str]
) -> tuple[set[str], set[str]]:
    """Role names whose .click() sits inside / outside with expect_http."""
    bound: dict[str, str] = {}
    inside: set[str] = set()
    outside: set[str] = set()

    def visit(node: ast.AST, in_expect: bool) -> None:
        if isinstance(node, ast.With):
            wrapped = in_expect or any(
                isinstance(item.context_expr, ast.Call)
                and isinstance(item.context_expr.func, ast.Attribute)
                and item.context_expr.func.attr == "expect_http"
                for item in node.items
            )
            for child in ast.iter_child_nodes(node):
                visit(child, wrapped)
            return
        if isinstance(node, ast.Assign) and isinstance(node.value, ast.Call):
            name = _role_name(node.value)
            if name:
                for target in node.targets:
                    if isinstance(target, ast.Name):
                        bound[target.id] = name
        if isinstance(node, ast.Call):
            label = _click_role_name(node, bound)
            if label in required:
                (inside if in_expect else outside).add(label)
        for child in ast.iter_child_nodes(node):
            visit(child, in_expect)

    for stmt in fn.body:
        visit(stmt, False)
    return inside, outside


class RecordDemoContractTests(unittest.TestCase):
    def test_human_testing_doc_mentions_each_scene(self):
        scenes = scene_keys(REC.read_text(encoding="utf-8"))
        self.assertTrue(scenes, "record_demo.py SCENES has no keys")
        table = _crate_scene_table(HUMAN_DOC.read_text(encoding="utf-8"))
        for scene in scenes:
            # Scene-column cell: `| `name` |` or compound `| `k8s` then `fleet` |`.
            # Extras prose after the table (`lab`, `kind`, `unify`) does not count.
            self.assertRegex(
                table,
                rf"(?m)^\|[^|\n]*\|[^|\n]*`{re.escape(scene)}`",
                f"docs/human-testing.md crate→scene table must include a | `{scene}` | cell",
            )

    def test_human_test_sh_pause_steps(self):
        keys = {
            _inventory_title(t)
            for t in _pause_head_titles(HUMAN_SH.read_text(encoding="utf-8"))
        }
        missing = sorted(_required_pause_titles() - keys)
        self.assertEqual(
            missing,
            [],
            "scripts/human-test.sh exact pause_step titles must cover "
            f"surface_map pauses ∪ ALWAYS_PAUSE; missing {missing}",
        )

    def test_surface_map_and_always_pause_titles_exist_as_exact_pause_steps(self):
        parsed = {
            _inventory_title(raw)
            for raw in _pause_head_titles(HUMAN_SH.read_text(encoding="utf-8"))
        }
        required = set(surface_scope.ALWAYS_PAUSE)
        for rule in surface_scope.load_rules():
            required.update(rule.get("pauses") or [])
        missing = sorted(required - parsed)
        self.assertEqual(
            missing,
            [],
            "load_rules() pauses ∪ ALWAYS_PAUSE must be a subset of parsed "
            f"pause_step titles; missing {missing}",
        )

    def test_chat_login_settings_pause_titles_are_not_satisfied_by_longer_siblings(self):
        keys = {
            _inventory_title(t)
            for t in _pause_head_titles(HUMAN_SH.read_text(encoding="utf-8"))
        }
        for short, longer in (
            ("Chat", "Chat history"),
            ("Login", "Login wall"),
            ("Settings", "Settings members"),
        ):
            self.assertIn(
                short,
                keys,
                f"pause_step titles must include {short!r}, not only {longer!r}",
            )

    def test_require_stack_uses_this_tree_identity(self):
        """Direct record_demo film fails closed on a foreign /health.git_sha."""
        import record_demo as rec
        from stack_identity import expected_sha

        foreign = "a" * 40
        want = expected_sha(REPO)
        self.assertNotEqual(want, foreign)
        httpd, url, _state = _serve_health(
            foreign, ["auth", "workspaces", "chat"]
        )
        prev_api = rec.API
        try:
            rec.API = url
            # --list is before the gate; a foreign stack must not block listing.
            self.assertEqual(rec.main(["--list"]), 0)
            with self.assertRaises(SystemExit) as ctx:
                rec.require_stack()
            err = str(ctx.exception)
            self.assertIn(foreign, err)
            self.assertIn(want, err)
            self.assertIn("/health.git_sha", err)
            # Film path (not --list) must hit the same gate before Playwright.
            with self.assertRaises(SystemExit) as film:
                rec.main(["chat"])
            self.assertIn(foreign, str(film.exception))
        finally:
            rec.API = prev_api
            httpd.shutdown()
            httpd.server_close()

    def test_human_test_sh_record_gates_this_tree_unless_list(self):
        """cmd_record must run this-tree identity unless argv is --list."""
        from test_human_surface_contract import _bash_fn

        cmd_record = _bash_fn(HUMAN_SH.read_text(encoding="utf-8"), "cmd_record")
        harness = (
            "set -euo pipefail\n"
            'TRACE="$1"\n'
            "shift\n"
            "need_cmd() { return 0; }\n"
            'require_this_tree() { echo IDENTITY >> "$TRACE"; return 0; }\n'
            'cmd_launch() { echo LAUNCH >> "$TRACE"; }\n'
            "warn() { :; }\n"
            'python3() { echo "PYTHON $*" >> "$TRACE"; return 0; }\n'
            'exec() { echo "EXEC $*" >> "$TRACE"; }\n'
            "REC=tests/browser/record_demo.py\n"
            f"{cmd_record}\n"
            'cmd_record "$@"\n'
        )

        def run(args: list[str]) -> str:
            with tempfile.TemporaryDirectory() as td:
                script = Path(td) / "record_gate.sh"
                trace = Path(td) / "trace"
                script.write_text(harness, encoding="utf-8")
                trace.write_text("", encoding="utf-8")
                ran = subprocess.run(
                    ["bash", str(script), str(trace), *args],
                    cwd=str(REPO),
                    capture_output=True,
                    text=True,
                    check=False,
                )
                self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
                return trace.read_text(encoding="utf-8")

        listed = run(["--list"])
        self.assertNotIn("IDENTITY", listed)
        self.assertNotIn("stack_identity.py", listed)
        self.assertIn("EXEC", listed)

        filmed = run(["chat"])
        self.assertTrue(
            "IDENTITY" in filmed or "stack_identity.py" in filmed,
            filmed,
        )
        self.assertIn("EXEC", filmed)
        self.assertNotIn("LAUNCH", filmed)

    def test_require_this_tree_fails_when_health_apis_omit_chat(self):
        """Matching bake without /health.apis 'chat' is still a foreign stack."""
        from stack_identity import ForeignStackError, expected_sha, require_this_tree

        want = expected_sha(REPO)
        httpd, url, state = _serve_health(want, ["auth", "workspaces"])
        try:
            with self.assertRaises(ForeignStackError) as ctx:
                require_this_tree(REPO, url, require_ui=False)
            self.assertIn("chat", str(ctx.exception))
            # Control: the same bake with chat is this tree (not every health fails).
            state["apis"] = ["auth", "workspaces", "chat"]
            body = require_this_tree(REPO, url, require_ui=False)
            self.assertEqual(body.get("git_sha"), want)
            self.assertIn("chat", body.get("apis") or [])
        finally:
            httpd.shutdown()
            httpd.server_close()

    def test_mutating_scene_clicks_wrap_expect_http(self):
        """Sign up / Start 2-task run / Create case / Start monitor / Invite
        must sit inside with expect_http — a name-scan of the helper stays green
        after every with-block is stripped.
        """
        src = REC.read_text(encoding="utf-8")
        tree = ast.parse(src)
        demo = next(
            n
            for n in tree.body
            if isinstance(n, ast.ClassDef) and n.name == "Demo"
        )
        fns = {
            n.name: n for n in demo.body if isinstance(n, ast.FunctionDef)
        }
        want = (
            ("signup_login", "Sign up"),
            ("scene_fleet", "Start 2-task run"),
            ("scene_cases", "Create case"),
            ("scene_inbox", "Start monitor"),
            ("scene_settings", "Invite"),
        )
        for fn_name, role in want:
            self.assertIn(fn_name, fns, f"Demo.{fn_name} missing")
            inside, outside = _clicks_wrapped_in_expect_http(fns[fn_name], {role})
            self.assertIn(
                role,
                inside,
                f"Demo.{fn_name}: {role!r} click must sit inside with expect_http",
            )
            self.assertNotIn(
                role,
                outside,
                f"Demo.{fn_name}: {role!r} click is outside expect_http",
            )

    def test_expect_http_missing_log_records_bug(self):
        import record_demo as rec

        with tempfile.TemporaryDirectory() as td:
            log = Path(td) / "fleet-server.log"
            demo = rec.Demo(None, Path(td))
            orig_mark, orig_since = rec.log_mark, rec.log_since
            rec.log_mark = lambda path=log: orig_mark(path)
            rec.log_since = lambda mark, path=log: orig_since(mark, path)
            try:
                with demo.expect_http(
                    "POST", "/api/auth/signup", status="201", timeout_s=0
                ):
                    pass
            finally:
                rec.log_mark = orig_mark
                rec.log_since = orig_since
            self.assertTrue(
                demo.findings,
                "expect_http must record a bug when the server log is missing",
            )
            line = demo.findings[-1]
            self.assertIn("POST", line)
            self.assertIn("/api/auth/signup", line)
            self.assertIn("201", line)

    def test_match_http_log_requires_method_and_uri_on_same_line(self):
        import record_demo as rec

        same = (
            "INFO request{method=POST uri=/api/auth/signup version=HTTP/1.1}: "
            "fleet_server: http response status=201 Created\n"
        )
        hit = rec.match_http_log(same, "POST", "/api/auth/signup", ("201",))
        self.assertIsNotNone(hit)
        self.assertIn("method=POST", hit)
        self.assertIn("/api/auth/signup", hit)
        self.assertIn("status=201", hit)

        split = (
            "INFO request{method=POST version=HTTP/1.1}\n"
            "INFO request{uri=/api/auth/signup}: "
            "fleet_server: http response status=201 Created\n"
        )
        self.assertIsNone(
            rec.match_http_log(split, "POST", "/api/auth/signup", ("201",))
        )
        wrong_method = (
            "INFO request{method=GET uri=/api/auth/signup version=HTTP/1.1}: "
            "fleet_server: http response status=201 Created\n"
        )
        self.assertIsNone(
            rec.match_http_log(wrong_method, "POST", "/api/auth/signup", ("201",))
        )

    def test_self_hosted_film_accepts_pending_last_seen_cards(self):
        text = REC.read_text(encoding="utf-8")
        self.assertIn(
            'filter(has_text="last seen")',
            text,
            "unify/runners film must treat pending last-seen cards as a list",
        )
        self.assertIn(
            '"pending" not in body.lower()',
            text,
            "pending pill is a listed worker, not a blank page",
        )

    def test_toast_card_does_not_eat_clicks_and_install_cmd_does_not_break_all(self):
        toast = (
            REPO
            / "crates"
            / "fleet-ui"
            / "src"
            / "components"
            / "toast.rs"
        ).read_text(encoding="utf-8")
        toast_prod = toast.split("#[cfg(test)]", 1)[0]
        page = (
            REPO
            / "crates"
            / "fleet-ui"
            / "src"
            / "pages"
            / "runners"
            / "self_hosted"
            / "page.rs"
        ).read_text(encoding="utf-8")
        self.assertNotIn(
            "pointer-events-auto relative overflow-hidden",
            toast_prod,
            "toast card must not intercept Apply under it",
        )
        self.assertIn("pointer-events-none relative overflow-hidden", toast_prod)
        self.assertNotIn(
            "break-all text-[11px]",
            page,
            "provider install curl|bash must not wrap mid-token",
        )
        self.assertIn("whitespace-pre", page)


if __name__ == "__main__":
    unittest.main(verbosity=2)
