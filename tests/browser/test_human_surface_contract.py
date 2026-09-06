#!/usr/bin/env python3
"""Contract: a user-visible surface needs a walkthrough step and a demo scene.

AGENTS.md / docs/human-testing.md / docs/testing-standardization.md: when you
add a page (or any user-visible or operator-visible path), add a `pause_step`
in scripts/human-test.sh and a SCENES entry in tests/browser/record_demo.py in
the **same change**. Adding a page without a scene is a miss. Agents record
`.webm` under `.tmp/demos/` and must never `git add` videos.

This is an inventory lock, not a snapshot of today's scene list. New
`crates/fleet-ui` page folders, `pub mod`s, `Route` paths, sidebar
`Route::` targets, and runner-nav children must grow both artifacts
automatically. Isolated cases prove a brand-new name (`zw_new_surface`)
is unsatisfied until scene + pause_step title + docs + CORE all exist.
A word in the Sidebar/Login pause body is not a step. Skip/alias maps
are frozen so a new name cannot be silently redirected onto an old
scene. Does not start the stack or film video.

  python3 -m unittest tests.browser.test_human_surface_contract -v
  python3 tests/browser/test_human_surface_contract.py -v
"""
from __future__ import annotations

import ast
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
REC = REPO / "tests" / "browser" / "record_demo.py"
HUMAN_DOC = REPO / "docs" / "human-testing.md"
HUMAN_SH = REPO / "scripts" / "human-test.sh"
EVERY = REPO / "scripts" / "run-every-test.sh"
PAGES = REPO / "crates" / "fleet-ui" / "src" / "pages"
UI_MAIN = REPO / "crates" / "fleet-ui" / "src" / "main.rs"
SIDEBAR = REPO / "crates" / "fleet-ui" / "src" / "components" / "sidebar.rs"
RUNNER_NAV = REPO / "crates" / "fleet-runners-view" / "src" / "routes.rs"

# Feature folders that are not a walkthrough surface.
# Frozen: do not add a skip to hide a missing scene.
SKIP_PAGE_DIRS = frozenset({"shared"})

# Historical folder → scene names that do not match the directory.
# A *new* page folder is not listed here: its scene name is the folder
# (or last segment). Do not add an alias to hide a missing scene.
PAGE_SCENE_ALIASES = {
    "home": "landing",
    "runners/self_hosted": "runners",
}

# OAuth popup is not a human-walk / record surface.
# Frozen: do not add a route skip to hide a missing scene.
NOT_WALKTHROUGH_ROUTES = frozenset({"/oauth/done"})

# Route last-segment → scene when the URL slug is not the scene key.
# Frozen: a new slug must become its own scene, not an alias onto landing.
ROUTE_SCENE_ALIASES = {
    "/": "landing",
    "login": "auth",
    "signup": "auth",
    "self-hosted": "runners",
}

# Auth is two pause_steps (Signup + Login); Chat history is the sessions page.
# Dashboard is walked on the Login step (no titled Dashboard step).
# Every other page scene must appear (word-boundary) in a pause_step title.
# Frozen: do not map a new scene onto Sidebar, Login, or an old pause title.
SCENE_PAUSE_NEEDLES: dict[str, tuple[str, ...]] = {
    "auth": ("Signup", "Login"),
    "sessions": ("Chat history",),
    "dashboard": ("Login",),
}

# Alias of runners — filmed via `workers` but not a distinct CORE page.
# Frozen: do not mark a new page as an alias to skip CORE.
ALIAS_SCENES = frozenset({"workers"})

# Route enum variant → scene when the ident is not the scene key.
# Frozen: a new variant (Billing, Azure) must become its own scene.
ROUTE_VARIANT_ALIASES = {
    "Home": "landing",
    "Login": "auth",
    "Signup": "auth",
    "SelfHosted": "runners",
    "CloudPage": "cloud",
    "PartnersPage": "partners",
    "RunnersIndex": "runners",
    "WorkersRedirect": "workers",
    "ChatThread": "chat",
    "SessionDetail": "sessions",
    "CaseDetail": "cases",
}

# Frozen: do not add a variant skip to hide a missing scene.
NOT_WALKTHROUGH_VARIANTS = frozenset({"OAuthDone"})

ROUTE_ATTR_RE = re.compile(r'#\[route\("([^"]+)"\)\]')
ROUTE_VARIANT_RE = re.compile(r"Route::([A-Z][A-Za-z0-9]*)")
PAUSE_HEAD_RE = re.compile(r'pause_step\s+"([^"]+)"')
SEGMENT_RE = re.compile(r'pub const SEGMENT_(\w+): &str = "([^"]+)"')
NAV_SLUG_RE = re.compile(r"slug:\s*SEGMENT_(\w+)")
WORD_RE_CACHE: dict[str, re.Pattern[str]] = {}


def _word_re(needle: str) -> re.Pattern[str]:
    cached = WORD_RE_CACHE.get(needle)
    if cached is None:
        cached = re.compile(
            rf"(?<![A-Za-z0-9_]){re.escape(needle)}(?![A-Za-z0-9_])",
            re.IGNORECASE,
        )
        WORD_RE_CACHE[needle] = cached
    return cached


def _assignment_value(src: str, name: str) -> ast.expr:
    tree = ast.parse(src)
    for node in tree.body:
        if isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
            if node.target.id == name and node.value is not None:
                return node.value
        if isinstance(node, ast.Assign):
            for target in node.targets:
                if isinstance(target, ast.Name) and target.id == name:
                    return node.value
    raise AssertionError(f"{name} assignment not found in record_demo.py")


def _string_list(value: ast.expr, name: str) -> list[str]:
    if not isinstance(value, (ast.List, ast.Tuple)):
        raise AssertionError(f"{name} is not a list/tuple literal")
    out: list[str] = []
    for elt in value.elts:
        if isinstance(elt, ast.Constant) and isinstance(elt.value, str):
            out.append(elt.value)
        else:
            raise AssertionError(f"non-string {name} entry: {ast.dump(elt)}")
    return out


def _string_dict_keys(value: ast.expr, name: str) -> list[str]:
    if not isinstance(value, ast.Dict):
        raise AssertionError(f"{name} is not a dict literal")
    keys: list[str] = []
    for k in value.keys:
        if isinstance(k, ast.Constant) and isinstance(k.value, str):
            keys.append(k.value)
        else:
            raise AssertionError(f"non-string {name} key: {ast.dump(k)}")
    return keys


def scene_keys(src: str) -> list[str]:
    return _string_dict_keys(_assignment_value(src, "SCENES"), "SCENES")


def core_scenes(src: str) -> list[str]:
    return _string_list(_assignment_value(src, "CORE"), "CORE")


def _leaf_surfaces(dir_path: Path, rel: str) -> list[str]:
    """Feature folders (and nested runners/*) that contain Rust sources."""
    subdirs = sorted(
        c
        for c in dir_path.iterdir()
        if c.is_dir() and not c.name.startswith(".") and c.name not in SKIP_PAGE_DIRS
    )
    child: list[str] = []
    for sub in subdirs:
        name = f"{rel}/{sub.name}" if rel else sub.name
        child.extend(_leaf_surfaces(sub, name))
    rs_files = [p for p in dir_path.glob("*.rs") if p.name != "mod.rs" or rel]
    own_page = (dir_path / "page.rs").is_file()
    if child:
        if own_page and rel:
            return [rel, *child]
        return child
    if rs_files and rel:
        return [rel]
    return []


def page_surfaces(root: Path | None = None) -> list[str]:
    """One entry per fleet-ui feature page (not shared helpers)."""
    pages = PAGES if root is None else root
    if not pages.is_dir():
        raise AssertionError(f"missing UI pages dir: {pages}")
    found = _leaf_surfaces(pages, "")
    for loose in sorted(pages.glob("*.rs")):
        if loose.name == "mod.rs":
            continue
        if loose.stem not in found:
            found.append(loose.stem)
    return found


def scene_for_page(page: str) -> str:
    if page in PAGE_SCENE_ALIASES:
        return PAGE_SCENE_ALIASES[page]
    return page.rsplit("/", 1)[-1]


def route_paths(src: str) -> list[str]:
    return ROUTE_ATTR_RE.findall(src)


def scene_for_route(path: str) -> str | None:
    if path in NOT_WALKTHROUGH_ROUTES:
        return None
    if path in ROUTE_SCENE_ALIASES:
        return ROUTE_SCENE_ALIASES[path]
    parts = [p for p in path.split("/") if p and not p.startswith(":")]
    if not parts:
        return "landing"
    if parts[0] in ROUTE_SCENE_ALIASES:
        return ROUTE_SCENE_ALIASES[parts[0]]
    if parts[0] == "runners":
        if len(parts) == 1:
            return "runners"
        child = parts[1]
        return ROUTE_SCENE_ALIASES.get(child, child)
    return parts[0]


def scene_for_route_variant(name: str) -> str | None:
    if name in NOT_WALKTHROUGH_VARIANTS:
        return None
    if name in ROUTE_VARIANT_ALIASES:
        return ROUTE_VARIANT_ALIASES[name]
    return re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()


def sidebar_route_variants(src: str) -> list[str]:
    seen: list[str] = []
    for name in ROUTE_VARIANT_RE.findall(src):
        if name not in seen:
            seen.append(name)
    return seen


def scene_bindings(src: str) -> dict[str, str]:
    """SCENES key → Demo.scene_* attribute name."""
    value = _assignment_value(src, "SCENES")
    if not isinstance(value, ast.Dict):
        raise AssertionError("SCENES is not a dict literal")
    out: dict[str, str] = {}
    for k, v in zip(value.keys, value.values):
        if not (isinstance(k, ast.Constant) and isinstance(k.value, str)):
            raise AssertionError(f"non-string SCENES key: {ast.dump(k)}")
        if not (
            isinstance(v, ast.Attribute)
            and isinstance(v.value, ast.Name)
            and v.value.id == "Demo"
        ):
            raise AssertionError(
                f'SCENES["{k.value}"] must be Demo.scene_* (not a lambda / other scene)'
            )
        out[k.value] = v.attr
    return out


def _collect_surface(
    label: str,
    scene: str,
    scene_set: set[str],
    core: set[str],
    steps: list[tuple[str, str]],
    doc: str,
) -> tuple[list[str], list[str], list[str], list[str]]:
    missing_scenes: list[str] = []
    missing_pauses: list[str] = []
    missing_docs: list[str] = []
    missing_core: list[str] = []
    _require_surface(
        label,
        scene,
        scene_set,
        core,
        steps,
        doc,
        missing_scenes,
        missing_pauses,
        missing_docs,
        missing_core,
    )
    return missing_scenes, missing_pauses, missing_docs, missing_core


def pause_steps(text: str) -> list[tuple[str, str]]:
    """Return (title, title+body) for each pause_step in cmd_guide."""
    matches = list(PAUSE_HEAD_RE.finditer(text))
    out: list[tuple[str, str]] = []
    for i, m in enumerate(matches):
        start = m.start()
        end = matches[i + 1].start() if i + 1 < len(matches) else len(text)
        blob = text[start:end]
        out.append((m.group(1), blob))
    return out


def pause_mentions(steps: list[tuple[str, str]], needle: str) -> bool:
    """True when needle appears (word-boundary) in a pause_step title."""
    pat = _word_re(needle)
    return any(pat.search(title) for title, _blob in steps)


def demo_scene_fns(src: str) -> dict[str, ast.FunctionDef]:
    tree = ast.parse(src)
    for node in tree.body:
        if isinstance(node, ast.ClassDef) and node.name == "Demo":
            out: dict[str, ast.FunctionDef] = {}
            for item in node.body:
                if isinstance(item, ast.FunctionDef) and item.name.startswith("scene_"):
                    out[item.name.removeprefix("scene_")] = item
            return out
    raise AssertionError("class Demo not found in record_demo.py")


def _fn_is_stub(fn: ast.FunctionDef) -> bool:
    body = [
        stmt
        for stmt in fn.body
        if not (
            isinstance(stmt, ast.Expr)
            and isinstance(stmt.value, ast.Constant)
            and isinstance(stmt.value.value, str)
        )
    ]
    if not body:
        return True
    return len(body) == 1 and isinstance(body[0], ast.Pass)


# Call names that count as filming the surface (AST Call.func, not comments).
_FILM_CALLS = frozenset({"goto", "ensure_session", "signup_login"})


def _call_name(func: ast.expr) -> str | None:
    if isinstance(func, ast.Name):
        return func.id
    if isinstance(func, ast.Attribute):
        return func.attr
    return None


def _fn_films(fn: ast.FunctionDef) -> bool:
    """True when the body calls a film helper — comments do not count."""
    for node in ast.walk(fn):
        if not isinstance(node, ast.Call):
            continue
        name = _call_name(node.func)
        if name is None:
            continue
        if name in _FILM_CALLS or name.startswith("_film_"):
            return True
    return False


def _page_route_scenes(ui: str) -> set[str]:
    required: set[str] = set()
    for page in page_surfaces():
        required.add(scene_for_page(page))
    for path in route_paths(ui):
        scene = scene_for_route(path)
        if scene is not None:
            required.add(scene)
    return required


def _scene_impl_gaps(
    scenes: set[str], fns: dict[str, ast.FunctionDef]
) -> tuple[list[str], list[str], list[str]]:
    missing: list[str] = []
    stubs: list[str] = []
    no_film: list[str] = []
    for scene in sorted(scenes):
        fn = fns.get(scene)
        if fn is None:
            missing.append(f"Demo.scene_{scene}")
        elif _fn_is_stub(fn):
            stubs.append(f"Demo.scene_{scene}")
        elif not _fn_films(fn):
            no_film.append(f"Demo.scene_{scene}")
    return missing, stubs, no_film


def runner_nav_slugs(src: str) -> list[str]:
    segments = dict(SEGMENT_RE.findall(src))
    slugs: list[str] = []
    for name in NAV_SLUG_RE.findall(src):
        if name not in segments:
            raise AssertionError(f"RUNNER_NAV_CHILDREN slug SEGMENT_{name} has no const")
        slugs.append(segments[name])
    return slugs


def _require_surface(
    label: str,
    scene: str,
    scene_set: set[str],
    core: set[str],
    steps: list[tuple[str, str]],
    doc: str,
    missing_scenes: list[str],
    missing_pauses: list[str],
    missing_docs: list[str],
    missing_core: list[str],
) -> None:
    if scene not in scene_set:
        missing_scenes.append(f"{label} → scene {scene!r}")
        return
    needles = SCENE_PAUSE_NEEDLES.get(scene, (scene,))
    for needle in needles:
        if not pause_mentions(steps, needle):
            missing_pauses.append(
                f"{label} → scene {scene!r} needs a pause_step title mentioning {needle!r}"
            )
    if not re.search(rf"`{re.escape(scene)}`|`{re.escape(scene)}\s", doc):
        missing_docs.append(f"{label} → `{scene}`")
    if scene not in ALIAS_SCENES and scene not in core:
        missing_core.append(
            f"{label} → scene {scene!r} must be in CORE so default record films it"
        )


def _bash_fn(text: str, name: str) -> str:
    """Product function body. Closing `}` must be at column 0."""
    m = re.search(rf"(?ms)^{re.escape(name)}\(\) \{{$.*?\n\}}", text)
    if m:
        return m.group(0)
    raise AssertionError(f"missing {name}()")


def _bash_case_arms(fn_src: str, var: str = "cmd") -> dict[str, str]:
    """Map `case "${var}" in` patterns to arm bodies."""
    m = re.search(
        rf'case\s+"\$\{{{re.escape(var)}\}}"\s+in\n(.*)\n\s*esac',
        fn_src,
        re.S,
    )
    if not m:
        raise AssertionError(f'missing case "${{{var}}}" in')
    arms: dict[str, str] = {}
    for chunk in m.group(1).split(";;"):
        chunk = chunk.strip()
        if not chunk:
            continue
        am = re.match(r"(.+?)\)\s*(.*)", chunk, re.S)
        if not am:
            continue
        arms[am.group(1).strip()] = am.group(2).strip()
    return arms


def _record_dispatch_src(sh: str) -> str:
    """main `record)` arm plus any function it calls (not a leftover cmd_record)."""
    arms = _bash_case_arms(_bash_fn(sh, "main"))
    body = None
    for pat, arm in arms.items():
        if "record" in {p.strip() for p in pat.split("|")}:
            body = arm
            break
    if body is None:
        raise AssertionError("human-test.sh main must have a record) command arm")
    chunks = [body]
    skip = {"exec", "if", "then", "else", "fi", "python3", "local", "return"}
    for name in re.findall(r"(?m)^\s*(\w+)", body):
        if name in skip:
            continue
        try:
            chunks.append(_bash_fn(sh, name))
        except AssertionError:
            continue
    return "\n".join(chunks)


def _list_scene_names(stdout: str) -> list[str]:
    """Indented scene keys from record_demo.py --list."""
    return [ln.strip() for ln in stdout.splitlines() if ln.startswith("  ")]


def _joined_str(node: ast.JoinedStr) -> str:
    parts: list[str] = []
    for v in node.values:
        if isinstance(v, ast.Constant) and isinstance(v.value, str):
            parts.append(v.value)
        elif isinstance(v, ast.FormattedValue) and isinstance(v.value, ast.Name):
            parts.append("{" + v.value.id + "}")
        else:
            parts.append("?")
    return "".join(parts)


def _record_demo_main(src: str) -> ast.FunctionDef:
    tree = ast.parse(src)
    for node in tree.body:
        if isinstance(node, ast.FunctionDef) and node.name == "main":
            return node
    raise AssertionError("record_demo.py main() not found")


def _every_run_steps(src: str) -> dict[str, str]:
    """Top-level `run_step name cmd` invocations (not the function)."""
    out: dict[str, str] = {}
    for part in re.split(r"(?m)^(?=run_step )", src):
        m = re.match(r"run_step (\S+)(?:\s+\\\s*\n\s*|\s+)(.*)", part, re.S)
        if not m:
            continue
        name = m.group(1)
        if "(" in name:
            continue
        cmd = " ".join(m.group(2).replace("\\\n", " ").split())
        out[name] = cmd
    return out


class HumanSurfaceContractTests(unittest.TestCase):
    def _assert_human_test_scope_dispatch(self) -> None:
        sh = HUMAN_SH.read_text(encoding="utf-8")
        arms = _bash_case_arms(_bash_fn(sh, "main"))
        self.assertIn('""', arms, 'human-test.sh must have a default "" command arm')
        self.assertIn("guide", arms, "human-test.sh must have a guide command arm")
        self.assertIn(
            "guide-all|walk",
            arms,
            "human-test.sh must have a guide-all|walk command arm",
        )
        self.assertRegex(
            arms['""'],
            r"(?m)^\s*load_scope_pauses\s*$",
            'default "" must invoke load_scope_pauses (not a leftover comment)',
        )
        self.assertRegex(
            arms["guide"],
            r"(?m)^\s*load_scope_pauses\s*$",
            "guide must invoke load_scope_pauses",
        )
        self.assertRegex(
            arms["guide-all|walk"],
            r"(?m)^\s*SCOPE_FILTER=0\s*$",
            "guide-all|walk must clear SCOPE_FILTER",
        )
        self.assertNotRegex(
            arms["guide-all|walk"],
            r"(?m)^\s*load_scope_pauses\s*$",
            "guide-all|walk must not re-apply the merge scope filter",
        )

    def _assert_record_demo_review_frames(self) -> None:
        main = _record_demo_main(REC.read_text(encoding="utf-8"))
        per_scene = False
        review_print = False
        for node in ast.walk(main):
            if (
                isinstance(node, ast.BinOp)
                and isinstance(node.op, ast.Div)
                and isinstance(node.left, ast.Name)
                and node.left.id == "frames_dir"
                and isinstance(node.right, ast.JoinedStr)
                and _joined_str(node.right) == "{name}.png"
            ):
                per_scene = True
            if (
                isinstance(node, ast.Call)
                and isinstance(node.func, ast.Name)
                and node.func.id == "print"
                and node.args
                and isinstance(node.args[0], ast.JoinedStr)
                and _joined_str(node.args[0]).startswith("REVIEW_FRAMES=")
            ):
                review_print = True
        self.assertTrue(
            per_scene,
            'record_demo.main must write frames_dir / f"{name}.png" '
            "(Demo.snap mid-action shots are not per-scene review frames)",
        )
        self.assertTrue(
            review_print,
            'record_demo.main must print f"REVIEW_FRAMES={frames_dir}"',
        )

    def _assert_every_test_scope_check(self) -> None:
        steps = _every_run_steps(EVERY.read_text(encoding="utf-8"))
        self.assertIn(
            "human-test-scope",
            steps,
            "run-every-test.sh must have run_step human-test-scope",
        )
        self.assertEqual(
            steps["human-test-scope"],
            "python3 tests/browser/surface_scope.py --check",
            "run_step human-test-scope must invoke surface_scope.py --check",
        )

    def test_film_and_human_test_are_scoped_to_the_merge(self):
        """Default/guide, per-scene review frames, and every-test --check."""
        self._assert_human_test_scope_dispatch()
        self._assert_record_demo_review_frames()
        self._assert_every_test_scope_check()

    def test_human_test_default_and_guide_call_load_scope_pauses_guide_all_clears_filter(
        self,
    ):
        self._assert_human_test_scope_dispatch()

    def test_record_demo_writes_per_scene_review_png_and_prints_REVIEW_FRAMES(self):
        self._assert_record_demo_review_frames()

    def test_every_test_run_step_human_test_scope_is_surface_scope_check(self):
        self._assert_every_test_scope_check()

    def test_demo_videos_are_not_git_added(self):
        rec_src = REC.read_text(encoding="utf-8")
        self.assertNotRegex(
            rec_src,
            r"""(["']git["']\s*,\s*["']add["'])|(subprocess\.\w+\([^)]*git\s+add)|(os\.system\([^)]*git\s+add)""",
            "record_demo.py must not invoke git add on demo videos",
        )

    def test_record_demo_default_out_is_repo_tmp_demos(self):
        main = _record_demo_main(REC.read_text(encoding="utf-8"))
        default = None
        for stmt in ast.walk(main):
            if not isinstance(stmt, ast.Assign):
                continue
            if not any(
                isinstance(t, ast.Name) and t.id == "out_dir" for t in stmt.targets
            ):
                continue
            value = stmt.value
            if not isinstance(value, ast.IfExp):
                continue
            test = value.test
            if not (
                isinstance(test, ast.Attribute)
                and isinstance(test.value, ast.Name)
                and test.value.id == "args"
                and test.attr == "out"
            ):
                continue
            default = value.orelse
            break
        self.assertIsNotNone(
            default,
            "record_demo.main must set out_dir from empty --out (if args.out else …)",
        )
        parts: list[str] = []
        cur = default
        while isinstance(cur, ast.BinOp) and isinstance(cur.op, ast.Div):
            if isinstance(cur.right, ast.Constant) and isinstance(cur.right.value, str):
                parts.append(cur.right.value)
            else:
                parts.append("?")
            cur = cur.left
        if isinstance(cur, ast.Name):
            parts.append(cur.id)
        parts.reverse()
        self.assertEqual(
            parts,
            ["REPO", ".tmp", "demos"],
            'empty --out branch must be REPO / ".tmp" / "demos"',
        )

    def test_git_check_ignore_covers_demo_webm(self):
        r = subprocess.run(
            ["git", "check-ignore", "-q", ".tmp/demos/x.webm"],
            cwd=REPO,
        )
        self.assertEqual(
            r.returncode,
            0,
            "git check-ignore must cover .tmp/demos/*.webm",
        )

    def test_no_tracked_webms(self):
        r = subprocess.run(
            ["git", "ls-files", "-z", "--", "*.webm"],
            cwd=REPO,
            check=True,
            capture_output=True,
        )
        tracked = [p for p in r.stdout.split(b"\0") if p]
        self.assertEqual(tracked, [], "no .webm may be tracked")

    def test_record_invokes_record_demo(self):
        """record) must reach exec of tests/browser/record_demo.py (any var name)."""
        sh = HUMAN_SH.read_text(encoding="utf-8")
        src = _record_dispatch_src(sh)
        execs = re.findall(r"exec\s+python3\s+(\S+)", src)
        self.assertTrue(
            execs,
            "record) arm must exec python3 of tests/browser/record_demo.py",
        )
        resolved: list[str] = []
        for raw in execs:
            tok = raw.strip("'\"")
            vm = re.fullmatch(r"\$\{(\w+)\}|\$(\w+)", tok)
            if vm:
                var = vm.group(1) or vm.group(2)
                am = re.search(rf'(?m)^{re.escape(var)}="([^"]*)"', sh)
                self.assertIsNotNone(
                    am,
                    f"record exec uses ${{{var}}} but {var}= is unset",
                )
                tok = am.group(1)
            resolved.append(tok)
        self.assertTrue(
            any("tests/browser/record_demo.py" in t for t in resolved),
            "record) arm must exec tests/browser/record_demo.py "
            f"(not a second recorder); resolved {resolved!r}",
        )

    def test_human_test_sh_record_list_stdout_matches_record_demo_list(self):
        """record --list must print the same scene names as record_demo.py --list."""
        via_sh = subprocess.run(
            [str(HUMAN_SH), "record", "--list"],
            cwd=REPO,
            capture_output=True,
            text=True,
            check=False,
            timeout=30,
        )
        self.assertEqual(via_sh.returncode, 0, via_sh.stdout + via_sh.stderr)
        via_py = subprocess.run(
            ["python3", str(REC), "--list"],
            cwd=REPO,
            capture_output=True,
            text=True,
            check=False,
            timeout=30,
        )
        self.assertEqual(via_py.returncode, 0, via_py.stdout + via_py.stderr)
        sh_names = _list_scene_names(via_sh.stdout)
        py_names = _list_scene_names(via_py.stdout)
        self.assertTrue(py_names, "record_demo.py --list printed no scene names")
        self.assertEqual(
            sh_names,
            py_names,
            "human-test.sh record --list must print the same scene names as "
            "python3 tests/browser/record_demo.py --list",
        )

    def test_cmd_record_list_does_not_call_cmd_launch(self):
        """${1:-} != --list must be the only path to cmd_launch / require_this_tree."""
        src = _record_dispatch_src(HUMAN_SH.read_text(encoding="utf-8"))
        guard = re.search(
            r'if \[\[ "\$\{1:-\}" != "--list" \]\]; then\n(.*?)\n  fi',
            src,
            re.S,
        )
        self.assertIsNotNone(
            guard,
            "record path must guard launch/identity with ${1:-} != --list",
        )
        inner = guard.group(1)
        outer = src[: guard.start()] + src[guard.end() :]
        self.assertRegex(
            inner,
            r"\b(cmd_launch|require_this_tree)\b",
            "the ${1:-} != --list branch must be the path to launch/identity",
        )
        self.assertNotRegex(
            outer,
            r"\bcmd_launch\b",
            "--list must not reach cmd_launch",
        )
        self.assertNotRegex(
            outer,
            r"\brequire_this_tree\b",
            "--list must not reach require_this_tree",
        )

    def test_skip_maps_cannot_hide_a_new_surface(self):
        """A new page name must demand its own scene — no silent skip."""
        self.assertEqual(SKIP_PAGE_DIRS, frozenset({"shared"}))
        self.assertEqual(
            PAGE_SCENE_ALIASES,
            {"home": "landing", "runners/self_hosted": "runners"},
        )
        self.assertEqual(NOT_WALKTHROUGH_ROUTES, frozenset({"/oauth/done"}))
        self.assertEqual(
            ROUTE_SCENE_ALIASES,
            {
                "/": "landing",
                "login": "auth",
                "signup": "auth",
                "self-hosted": "runners",
            },
        )
        self.assertEqual(
            SCENE_PAUSE_NEEDLES,
            {
                "auth": ("Signup", "Login"),
                "sessions": ("Chat history",),
                "dashboard": ("Login",),
            },
        )
        self.assertEqual(ALIAS_SCENES, frozenset({"workers"}))
        self.assertEqual(
            ROUTE_VARIANT_ALIASES,
            {
                "Home": "landing",
                "Login": "auth",
                "Signup": "auth",
                "SelfHosted": "runners",
                "CloudPage": "cloud",
                "PartnersPage": "partners",
                "RunnersIndex": "runners",
                "WorkersRedirect": "workers",
                "ChatThread": "chat",
                "SessionDetail": "sessions",
                "CaseDetail": "cases",
            },
        )
        self.assertEqual(NOT_WALKTHROUGH_VARIANTS, frozenset({"OAuthDone"}))
        self.assertEqual(scene_for_page("billing"), "billing")
        self.assertEqual(scene_for_page("admin/reports"), "reports")
        self.assertEqual(scene_for_route("/:workspace_id/billing"), "billing")
        self.assertEqual(scene_for_route("/:workspace_id/runners/azure"), "azure")
        self.assertEqual(scene_for_route("/oauth/done"), None)
        self.assertEqual(scene_for_route_variant("Billing"), "billing")
        self.assertEqual(scene_for_route_variant("AdminReports"), "admin_reports")
        self.assertEqual(scene_for_route_variant("OAuthDone"), None)
        self.assertNotIn("billing", SKIP_PAGE_DIRS)
        self.assertNotIn("billing", PAGE_SCENE_ALIASES)
        self.assertNotIn("/:workspace_id/billing", NOT_WALKTHROUGH_ROUTES)
        self.assertNotIn("billing", ROUTE_SCENE_ALIASES)
        self.assertNotIn("azure", ROUTE_SCENE_ALIASES)
        self.assertNotIn("billing", ALIAS_SCENES)
        self.assertNotIn("Billing", ROUTE_VARIANT_ALIASES)
        self.assertNotIn("Azure", ROUTE_VARIANT_ALIASES)
        self.assertNotIn("Billing", NOT_WALKTHROUGH_VARIANTS)

    def test_user_visible_pages_have_scene_step_and_docs(self):
        rec_src = REC.read_text(encoding="utf-8")
        doc = HUMAN_DOC.read_text(encoding="utf-8")
        sh = HUMAN_SH.read_text(encoding="utf-8")
        scenes = scene_keys(rec_src)
        scene_set = set(scenes)
        core = set(core_scenes(rec_src))
        steps = pause_steps(sh)
        self.assertTrue(steps, "scripts/human-test.sh must pause_step through the product")

        pages = page_surfaces()
        self.assertGreaterEqual(
            len(pages),
            10,
            f"page inventory looks empty: {pages}",
        )
        missing_scenes: list[str] = []
        missing_pauses: list[str] = []
        missing_docs: list[str] = []
        missing_core: list[str] = []
        for page in pages:
            _require_surface(
                page,
                scene_for_page(page),
                scene_set,
                core,
                steps,
                doc,
                missing_scenes,
                missing_pauses,
                missing_docs,
                missing_core,
            )
        self.assertEqual(
            missing_scenes,
            [],
            "user-visible page folder has no record_demo.py SCENES entry "
            "(add the scene and the human-test.sh step in the same change): "
            + "; ".join(missing_scenes),
        )
        self.assertEqual(
            missing_pauses,
            [],
            "user-visible page has no scripts/human-test.sh pause_step "
            "(add the step and the record_demo.py scene in the same change): "
            + "; ".join(missing_pauses),
        )
        self.assertEqual(
            missing_docs,
            [],
            "docs/human-testing.md crate→scene table must mention each page scene: "
            + "; ".join(missing_docs),
        )
        self.assertEqual(
            missing_core,
            [],
            "user-visible page scene must be in record_demo CORE "
            f"(not an extra-only scene): {missing_core}",
        )

    def test_user_visible_routes_have_scene_and_step(self):
        rec_src = REC.read_text(encoding="utf-8")
        doc = HUMAN_DOC.read_text(encoding="utf-8")
        sh = HUMAN_SH.read_text(encoding="utf-8")
        ui = UI_MAIN.read_text(encoding="utf-8")
        scene_set = set(scene_keys(rec_src))
        core = set(core_scenes(rec_src))
        steps = pause_steps(sh)
        routes = route_paths(ui)
        self.assertGreaterEqual(len(routes), 10, f"Route inventory looks empty: {routes}")
        missing_scenes: list[str] = []
        missing_pauses: list[str] = []
        missing_docs: list[str] = []
        missing_core: list[str] = []
        for path in routes:
            scene = scene_for_route(path)
            if scene is None:
                continue
            _require_surface(
                path,
                scene,
                scene_set,
                core,
                steps,
                doc,
                missing_scenes,
                missing_pauses,
                missing_docs,
                missing_core,
            )
        self.assertEqual(
            missing_scenes,
            [],
            "user-visible Route has no record_demo.py SCENES entry "
            "(adding a page without a scene is a miss): "
            + "; ".join(missing_scenes),
        )
        self.assertEqual(
            missing_pauses,
            [],
            "user-visible Route has no scripts/human-test.sh pause_step "
            "(same change as the record_demo.py scene): "
            + "; ".join(missing_pauses),
        )
        self.assertEqual(
            missing_docs,
            [],
            "docs/human-testing.md must mention each Route scene: "
            + "; ".join(missing_docs),
        )
        self.assertEqual(
            missing_core,
            [],
            "user-visible Route scene must be in record_demo CORE "
            f"(not an extra-only scene): {missing_core}",
        )

    def test_runner_nav_children_have_scenes(self):
        rec_src = REC.read_text(encoding="utf-8")
        doc = HUMAN_DOC.read_text(encoding="utf-8")
        sh = HUMAN_SH.read_text(encoding="utf-8")
        nav = RUNNER_NAV.read_text(encoding="utf-8")
        slugs = runner_nav_slugs(nav)
        self.assertGreaterEqual(len(slugs), 3, f"runner nav looks empty: {slugs}")
        scene_set = set(scene_keys(rec_src))
        core = set(core_scenes(rec_src))
        steps = pause_steps(sh)
        missing_scenes: list[str] = []
        missing_pauses: list[str] = []
        missing_docs: list[str] = []
        missing_core: list[str] = []
        for slug in slugs:
            path = f"/:workspace_id/runners/{slug}"
            scene = scene_for_route(path)
            self.assertIsNotNone(scene, path)
            _require_surface(
                f"RUNNER_NAV_CHILDREN {slug}",
                scene,  # type: ignore[arg-type]
                scene_set,
                core,
                steps,
                doc,
                missing_scenes,
                missing_pauses,
                missing_docs,
                missing_core,
            )
        self.assertEqual(
            missing_scenes,
            [],
            "sidebar runner child has no record_demo.py SCENES entry "
            "(same change as the human-test.sh step): "
            + "; ".join(missing_scenes),
        )
        self.assertEqual(
            missing_pauses,
            [],
            "sidebar runner child has no scripts/human-test.sh pause_step: "
            + "; ".join(missing_pauses),
        )
        self.assertEqual(
            missing_docs,
            [],
            "docs/human-testing.md must mention each runner-nav scene: "
            + "; ".join(missing_docs),
        )
        self.assertEqual(
            missing_core,
            [],
            f"runner-nav scene must be in CORE: {missing_core}",
        )

    def test_required_scenes_are_implemented(self):
        rec_src = REC.read_text(encoding="utf-8")
        ui = UI_MAIN.read_text(encoding="utf-8")
        fns = demo_scene_fns(rec_src)
        required = _page_route_scenes(ui)
        self.assertTrue(required, "surface inventory produced no scenes")
        missing, stubs, no_film = _scene_impl_gaps(required, fns)
        self.assertEqual(
            missing,
            [],
            "record_demo.py must define Demo.scene_* for each required surface: "
            + "; ".join(missing),
        )
        self.assertEqual(
            stubs,
            [],
            "record_demo.py scene must not be `pass` / docstring-only: "
            + "; ".join(stubs),
        )
        self.assertEqual(
            no_film,
            [],
            "Demo.scene_* must call a film helper (goto / ensure_session / "
            "signup_login / _film_*), not a comment: "
            + "; ".join(no_film),
        )

    def test_scenes_dict_extras_are_implemented(self):
        """SCENES/CORE extras (shell, workspace, api, lab, kind, k8s, unify)
        are not page/route-derived; they still need a filming Demo.scene_*."""
        rec_src = REC.read_text(encoding="utf-8")
        ui = UI_MAIN.read_text(encoding="utf-8")
        fns = demo_scene_fns(rec_src)
        extras = (set(scene_keys(rec_src)) | set(core_scenes(rec_src))) - _page_route_scenes(
            ui
        )
        self.assertTrue(
            extras,
            "SCENES/CORE extras (shell, workspace, api, lab, kind, k8s, unify) "
            "must exist beyond page/route surfaces",
        )
        missing, stubs, no_film = _scene_impl_gaps(extras, fns)
        self.assertEqual(
            missing,
            [],
            "SCENES/CORE extra must define Demo.scene_*: " + "; ".join(missing),
        )
        self.assertEqual(
            stubs,
            [],
            "SCENES/CORE extra must not be `pass` / docstring-only: "
            + "; ".join(stubs),
        )
        self.assertEqual(
            no_film,
            [],
            "SCENES/CORE extra must call a film helper: " + "; ".join(no_film),
        )

    def test_scenes_dict_maps_to_matching_demo_method(self):
        """A new key cannot reuse Demo.scene_landing to fake coverage."""
        bindings = scene_bindings(REC.read_text(encoding="utf-8"))
        self.assertGreaterEqual(len(bindings), 10, bindings)
        for scene, attr in bindings.items():
            if scene in ALIAS_SCENES:
                self.assertIn(
                    attr,
                    {f"scene_{scene}", "scene_runners"},
                    f'SCENES["{scene}"] may alias scene_runners, not an unrelated scene',
                )
                continue
            self.assertEqual(
                attr,
                f"scene_{scene}",
                f'SCENES["{scene}"] must be Demo.scene_{scene}',
            )

    def test_sidebar_nav_routes_have_scenes(self):
        rec_src = REC.read_text(encoding="utf-8")
        doc = HUMAN_DOC.read_text(encoding="utf-8")
        sh = HUMAN_SH.read_text(encoding="utf-8")
        variants = sidebar_route_variants(SIDEBAR.read_text(encoding="utf-8"))
        self.assertGreaterEqual(
            len(variants),
            8,
            f"sidebar Route:: inventory looks empty: {variants}",
        )
        scene_set = set(scene_keys(rec_src))
        core = set(core_scenes(rec_src))
        steps = pause_steps(sh)
        missing_scenes: list[str] = []
        missing_pauses: list[str] = []
        missing_docs: list[str] = []
        missing_core: list[str] = []
        for name in variants:
            scene = scene_for_route_variant(name)
            if scene is None:
                continue
            _require_surface(
                f"sidebar Route::{name}",
                scene,
                scene_set,
                core,
                steps,
                doc,
                missing_scenes,
                missing_pauses,
                missing_docs,
                missing_core,
            )
        self.assertEqual(
            missing_scenes,
            [],
            "sidebar Route:: target has no record_demo.py SCENES entry "
            "(add the scene and the human-test.sh step in the same change): "
            + "; ".join(missing_scenes),
        )
        self.assertEqual(
            missing_pauses,
            [],
            "sidebar Route:: target has no scripts/human-test.sh pause_step: "
            + "; ".join(missing_pauses),
        )
        self.assertEqual(
            missing_docs,
            [],
            "docs/human-testing.md must mention each sidebar scene: "
            + "; ".join(missing_docs),
        )
        self.assertEqual(
            missing_core,
            [],
            f"sidebar scene must be in CORE: {missing_core}",
        )

    def test_page_inventory_picks_up_a_new_folder(self):
        """Public page_surfaces() walks an injected root, not a hardcoded list."""
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            (root / "shared").mkdir()
            (root / "shared" / "empty.rs").write_text("x\n", encoding="utf-8")
            (root / "billing").mkdir()
            (root / "billing" / "page.rs").write_text("x\n", encoding="utf-8")
            (root / "runners" / "azure").mkdir(parents=True)
            (root / "runners" / "azure" / "page.rs").write_text("x\n", encoding="utf-8")
            found = page_surfaces(root)
        self.assertIn("billing", found)
        self.assertIn("runners/azure", found)
        self.assertNotIn("shared", found)

    def test_page_surfaces_includes_parent_when_page_rs_and_children(self):
        """A dir with page.rs plus a nested page.rs yields both rel and child."""
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            (root / "runners").mkdir()
            (root / "runners" / "page.rs").write_text("x\n", encoding="utf-8")
            (root / "runners" / "azure").mkdir()
            (root / "runners" / "azure" / "page.rs").write_text("x\n", encoding="utf-8")
            found = page_surfaces(root)
        self.assertIn("runners", found)
        self.assertIn("runners/azure", found)

    def test_page_surfaces_appends_loose_rs_not_mod(self):
        """A root-level reports.rs appears via page_surfaces itself, not a copied glob."""
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            (root / "reports.rs").write_text("x\n", encoding="utf-8")
            (root / "mod.rs").write_text("x\n", encoding="utf-8")
            found = page_surfaces(root)
        self.assertIn("reports", found)
        self.assertNotIn("mod", found)

    def test_new_surface_needs_scene_step_docs_and_core(self):
        """Same change: a new page is red until all four artifacts exist.

        Control: once scene + pause_step title + docs + CORE are present,
        green — so the lock is not 'every unknown name fails forever'.
        """
        probe = "zw_new_surface"
        rec_src = REC.read_text(encoding="utf-8")
        real_scenes = set(scene_keys(rec_src))
        real_core = set(core_scenes(rec_src))
        real_steps = pause_steps(HUMAN_SH.read_text(encoding="utf-8"))
        real_doc = HUMAN_DOC.read_text(encoding="utf-8")
        self.assertNotIn(probe, real_scenes)
        self.assertNotIn(probe, real_core)
        self.assertNotIn(probe, SCENE_PAUSE_NEEDLES)
        self.assertFalse(pause_mentions(real_steps, probe))

        ms, mp, md, mc = _collect_surface(
            f"pages/{probe}", probe, real_scenes, real_core, real_steps, real_doc
        )
        self.assertTrue(
            ms,
            "a page folder with no SCENES entry must fail (adding a page "
            "without a scene is a miss)",
        )

        ready_doc = real_doc + f"\n`{probe}`\n"
        ready_steps = real_steps + [(probe, f'pause_step "{probe}"\n')]
        ms, mp, md, mc = _collect_surface(
            f"pages/{probe}",
            probe,
            real_scenes | {probe},
            real_core | {probe},
            ready_steps,
            ready_doc,
        )
        self.assertEqual(ms, [])
        self.assertEqual(mp, [])
        self.assertEqual(md, [])
        self.assertEqual(mc, [])

    def test_new_leaf_page_folder_is_red_in_require_surface(self):
        """A new pages/<name>/page.rs is red against live rec/human files."""
        probe = "zw_new_surface"
        rec_src = REC.read_text(encoding="utf-8")
        scene_set = set(scene_keys(rec_src))
        core = set(core_scenes(rec_src))
        steps = pause_steps(HUMAN_SH.read_text(encoding="utf-8"))
        doc = HUMAN_DOC.read_text(encoding="utf-8")
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            (root / probe).mkdir()
            (root / probe / "page.rs").write_text("x\n", encoding="utf-8")
            found = page_surfaces(root)
        self.assertIn(probe, found)
        missing_scenes: list[str] = []
        missing_pauses: list[str] = []
        missing_docs: list[str] = []
        missing_core: list[str] = []
        _require_surface(
            probe,
            scene_for_page(probe),
            scene_set,
            core,
            steps,
            doc,
            missing_scenes,
            missing_pauses,
            missing_docs,
            missing_core,
        )
        self.assertTrue(
            missing_scenes,
            "page_surfaces-discovered folder must fail _require_surface "
            "until SCENES lists it",
        )

    def test_docs_plain_probe_word_without_backticks_still_missing(self):
        """A bare scene name in docs/human-testing.md is not a `{scene}` row."""
        probe = "zw_new_surface"
        rec_src = REC.read_text(encoding="utf-8")
        real_scenes = set(scene_keys(rec_src))
        real_core = set(core_scenes(rec_src))
        real_steps = pause_steps(HUMAN_SH.read_text(encoding="utf-8"))
        ready_doc = HUMAN_DOC.read_text(encoding="utf-8")
        ready_doc += f"\n{probe}\n"
        ready_steps = real_steps + [(probe, f'pause_step "{probe}"\n')]
        ms, mp, md, mc = _collect_surface(
            f"pages/{probe}",
            probe,
            real_scenes | {probe},
            real_core | {probe},
            ready_steps,
            ready_doc,
        )
        self.assertTrue(
            md,
            "plain probe word without backticks must still report missing_docs",
        )

    def test_required_scene_comment_goto_is_not_implementation(self):
        tree = ast.parse(
            "class Demo:\n"
            "    def scene_billing(self):\n"
            "        '''docs only'''\n"
            "        # goto billing\n"
            "        return\n"
        )
        demo = next(n for n in tree.body if isinstance(n, ast.ClassDef))
        fn = next(n for n in demo.body if isinstance(n, ast.FunctionDef))
        self.assertFalse(_fn_is_stub(fn))
        self.assertFalse(
            _fn_films(fn),
            "a '# goto billing' comment plus return is not a film Call",
        )
        missing, stubs, no_film = _scene_impl_gaps({"billing"}, {"billing": fn})
        self.assertEqual(missing, [])
        self.assertEqual(stubs, [])
        self.assertEqual(no_film, ["Demo.scene_billing"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
