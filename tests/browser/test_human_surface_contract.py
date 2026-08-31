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
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
REC = REPO / "tests" / "browser" / "record_demo.py"
HUMAN_DOC = REPO / "docs" / "human-testing.md"
HUMAN_SH = REPO / "scripts" / "human-test.sh"
AGENTS = REPO / "AGENTS.md"
TESTING_STD = REPO / "docs" / "testing-standardization.md"
EVERY = REPO / "scripts" / "run-every-test.sh"
PAGES = REPO / "crates" / "fleet-ui" / "src" / "pages"
PAGES_MOD = PAGES / "mod.rs"
RUNNERS_MOD = PAGES / "runners" / "mod.rs"
UI_MAIN = REPO / "crates" / "fleet-ui" / "src" / "main.rs"
SIDEBAR = REPO / "crates" / "fleet-ui" / "src" / "components" / "sidebar.rs"
RUNNER_NAV = REPO / "crates" / "fleet-runners-view" / "src" / "routes.rs"
GITIGNORE = REPO / ".gitignore"

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
PUB_MOD_RE = re.compile(r"(?m)^pub mod (\w+);")
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


def pub_mods(path: Path) -> list[str]:
    if not path.is_file():
        raise AssertionError(f"missing module file: {path}")
    return PUB_MOD_RE.findall(path.read_text(encoding="utf-8"))


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


def page_surfaces() -> list[str]:
    """One entry per fleet-ui feature page (not shared helpers)."""
    if not PAGES.is_dir():
        raise AssertionError(f"missing UI pages dir: {PAGES}")
    found = _leaf_surfaces(PAGES, "")
    for loose in sorted(PAGES.glob("*.rs")):
        if loose.name == "mod.rs":
            continue
        if loose.stem not in found:
            found.append(loose.stem)
    return found


def declared_page_mods() -> list[str]:
    """`pub mod` inventory from pages/mod.rs (+ runners/mod.rs children)."""
    out: list[str] = []
    for name in pub_mods(PAGES_MOD):
        if name in SKIP_PAGE_DIRS:
            continue
        if name == "runners":
            if not RUNNERS_MOD.is_file():
                raise AssertionError(f"missing {RUNNERS_MOD}")
            for child in pub_mods(RUNNERS_MOD):
                out.append(f"runners/{child}")
            continue
        out.append(name)
    return out


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


class HumanSurfaceContractTests(unittest.TestCase):
    def test_artifacts_exist(self):
        self.assertTrue(REC.is_file(), REC)
        self.assertTrue(HUMAN_DOC.is_file(), HUMAN_DOC)
        self.assertTrue(HUMAN_SH.is_file(), HUMAN_SH)
        self.assertTrue(AGENTS.is_file(), AGENTS)
        self.assertTrue(TESTING_STD.is_file(), TESTING_STD)
        self.assertTrue(UI_MAIN.is_file(), UI_MAIN)
        self.assertTrue(SIDEBAR.is_file(), SIDEBAR)
        self.assertTrue(PAGES.is_dir(), PAGES)
        self.assertTrue(PAGES_MOD.is_file(), PAGES_MOD)
        self.assertTrue(RUNNER_NAV.is_file(), RUNNER_NAV)
        self.assertTrue(
            HUMAN_SH.stat().st_mode & 0o111,
            "human-test.sh must be executable",
        )

    def test_policy_docs_require_same_change(self):
        """Adding a page without a scene is a miss — same change, not later."""
        for path, body in (
            (HUMAN_DOC, HUMAN_DOC.read_text(encoding="utf-8")),
            (AGENTS, AGENTS.read_text(encoding="utf-8")),
            (TESTING_STD, TESTING_STD.read_text(encoding="utf-8")),
        ):
            self.assertRegex(
                body,
                r"same.{0,10}change",
                f"{path.relative_to(REPO)} must require the step and scene "
                "in the same change",
            )
            self.assertIn("human-test.sh", body, path.name)
            self.assertIn("record_demo.py", body, path.name)

        doc = HUMAN_DOC.read_text(encoding="utf-8")
        self.assertRegex(
            doc,
            r"Adding a scene",
            "docs/human-testing.md must tell authors how to add a scene when adding a surface",
        )
        self.assertIn("SCENES", doc)
        self.assertRegex(doc, r"pause-step|pause_step")
        self.assertRegex(
            doc,
            r"no step for it.*incomplete|change is incomplete",
            "walkthrough must treat a missing step as an incomplete change",
        )
        std = TESTING_STD.read_text(encoding="utf-8")
        self.assertRegex(
            std,
            r"New UI or operator surface",
            "testing-standardization.md must classify a new surface as "
            "e2e + human-test step + record scene",
        )
        self.assertRegex(
            std,
            r"walkthrough later",
            "testing-standardization.md must reject deferring the walkthrough "
            "(that phrase belongs in the Not-here column)",
        )

    def test_film_and_human_test_are_scoped_to_the_merge(self):
        """Clip + walkthrough: final merge tree, HUMAN_TEST_SCOPE.md, film review."""
        for path in (HUMAN_DOC, AGENTS, TESTING_STD):
            body = path.read_text(encoding="utf-8")
            self.assertRegex(
                body,
                r"final (tree that will be merged|merge tree)",
                f"{path.relative_to(REPO)} must require filming the final "
                "version that will be merged",
            )
            self.assertRegex(
                body,
                r"not a\s+random",
                f"{path.relative_to(REPO)} must reject a random walkthrough",
            )
            self.assertIn(
                "HUMAN_TEST_SCOPE.md",
                body,
                f"{path.relative_to(REPO)} must require the MR scope file",
            )
            self.assertRegex(
                body,
                r"(?i)review the film",
                f"{path.relative_to(REPO)} must require a post-record film review",
            )

        self.assertIn(
            "surface_scope.py --check",
            EVERY.read_text(encoding="utf-8"),
            "run-every-test.sh must check HUMAN_TEST_SCOPE.md against the diff",
        )
        rec = REC.read_text(encoding="utf-8")
        self.assertIn("HUMAN_TEST_SCOPE.md", rec)
        self.assertIn("REVIEW_FRAMES", rec)
        self.assertIn("page.screenshot", rec)
        sh = HUMAN_SH.read_text(encoding="utf-8")
        self.assertIn("HUMAN_TEST_SCOPE.md", sh)
        self.assertIn("load_scope_pauses", sh)
        self.assertIn("guide-all", sh)

    def test_this_mr_step_requires_step_and_scene(self):
        sh = HUMAN_SH.read_text(encoding="utf-8")
        steps = pause_steps(sh)
        self.assertTrue(steps, "scripts/human-test.sh must pause_step through the product")
        mr = next((blob for title, blob in steps if title.startswith("This merge request")), "")
        self.assertTrue(
            mr,
            'human-test.sh must pause_step "This merge request" and tell the human '
            "to add a step + scene when the new surface is missing",
        )
        self.assertIn("scripts/human-test.sh", mr)
        self.assertIn("tests/browser/record_demo.py", mr)
        self.assertRegex(mr, r"same change")

    def test_demo_videos_are_not_git_added(self):
        doc = HUMAN_DOC.read_text(encoding="utf-8")
        rec_src = REC.read_text(encoding="utf-8")
        agents = AGENTS.read_text(encoding="utf-8")
        self.assertRegex(
            doc,
            r"Never commit videos|Do \*\*not\*\* `git add` videos|do not `git add` videos",
            "docs/human-testing.md must forbid git-adding demo videos",
        )
        self.assertRegex(
            agents,
            r"Never.{0,20}git add.{0,20}videos",
            "AGENTS.md must forbid git-adding demo videos",
        )
        self.assertIn(".tmp/demos", rec_src)
        self.assertIn(".webm", rec_src)
        self.assertIn(".tmp/demos", HUMAN_SH.read_text(encoding="utf-8"))
        self.assertRegex(
            rec_src,
            r"Do not git add|do not git add",
            "record_demo.py must tell the agent not to git add the .webm",
        )
        self.assertNotRegex(
            rec_src,
            r"""(["']git["']\s*,\s*["']add["'])|(subprocess\.\w+\([^)]*git\s+add)|(os\.system\([^)]*git\s+add)""",
            "record_demo.py must not invoke git add on demo videos",
        )
        ignore = GITIGNORE.read_text(encoding="utf-8")
        self.assertRegex(
            ignore,
            r"(?m)^\s*\.tmp/",
            ".tmp/ (demo videos) must stay gitignored",
        )

    def test_record_invokes_record_demo(self):
        sh = HUMAN_SH.read_text(encoding="utf-8")
        self.assertIn('REC="${ROOT}/tests/browser/record_demo.py"', sh)
        self.assertRegex(
            sh,
            r'exec python3 "\$\{REC\}"',
            "human-test.sh record must exec record_demo.py (not a second recorder)",
        )

    def test_every_test_runs_this_contract(self):
        text = EVERY.read_text(encoding="utf-8")
        self.assertIn(
            "tests.browser.test_human_surface_contract",
            text,
            "run-every-test.sh must run this inventory lock (sibling of "
            "test_record_demo_contract) so a new page without a scene fails closed",
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

    def test_declared_page_mods_are_inventoried(self):
        folders = set(page_surfaces())
        mods = declared_page_mods()
        self.assertGreaterEqual(len(mods), 10, f"pub mod inventory looks empty: {mods}")
        missing = [m for m in mods if m not in folders]
        self.assertEqual(
            missing,
            [],
            "pages/mod.rs pub mod has no feature folder with .rs "
            "(a new module is a surface — add the folder, scene, and pause_step): "
            + "; ".join(missing),
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
        required: set[str] = set()
        for page in page_surfaces():
            required.add(scene_for_page(page))
        for path in route_paths(ui):
            scene = scene_for_route(path)
            if scene is not None:
                required.add(scene)
        self.assertTrue(required, "surface inventory produced no scenes")
        stubs: list[str] = []
        missing: list[str] = []
        for scene in sorted(required):
            self.assertIn(
                f'"{scene}"',
                rec_src,
                f'SCENES must register "{scene}"',
            )
            fn = fns.get(scene)
            if fn is None:
                missing.append(f"Demo.scene_{scene}")
                continue
            if _fn_is_stub(fn):
                stubs.append(f"Demo.scene_{scene}")
                continue
            chunk = ast.get_source_segment(rec_src, fn) or ""
            self.assertRegex(
                chunk,
                r"goto|ensure_session|_film_|signup_login",
                f"Demo.scene_{scene} must film the surface "
                "(goto / ensure_session / signup_login), not a comment-only stub",
            )
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
        """Inventory is derived from the tree, not a hardcoded page list."""
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            (root / "shared").mkdir()
            (root / "shared" / "empty.rs").write_text("x\n", encoding="utf-8")
            (root / "billing").mkdir()
            (root / "billing" / "page.rs").write_text("x\n", encoding="utf-8")
            (root / "runners" / "azure").mkdir(parents=True)
            (root / "runners" / "azure" / "page.rs").write_text("x\n", encoding="utf-8")
            (root / "reports.rs").write_text("x\n", encoding="utf-8")
            found = _leaf_surfaces(root, "")
            for loose in sorted(root.glob("*.rs")):
                if loose.name != "mod.rs" and loose.stem not in found:
                    found.append(loose.stem)
        self.assertIn("billing", found)
        self.assertIn("runners/azure", found)
        self.assertIn("reports", found)
        self.assertNotIn("shared", found)
        self.assertEqual(scene_for_page("billing"), "billing")
        self.assertEqual(scene_for_page("runners/azure"), "azure")

    def test_pause_mentions_counts_titles_not_bodies(self):
        """A nav label in Sidebar/Login copy is not a walkthrough step."""
        steps = [
            (
                "Sidebar",
                'pause_step "Sidebar"\n'
                '    "Monitoring: Dashboard, Fleet, zw_new_surface."\n',
            ),
            (
                "Login  ${UI_URL}/login",
                'pause_step "Login  ${UI_URL}/login"\n'
                '    "lands on /{ws}/dashboard zw_new_surface."\n',
            ),
        ]
        self.assertFalse(pause_mentions(steps, "zw_new_surface"))
        self.assertFalse(pause_mentions(steps, "dashboard"))
        self.assertTrue(pause_mentions(steps, "Sidebar"))
        self.assertTrue(pause_mentions(steps, "Login"))
        titled = steps + [("zw_new_surface", 'pause_step "zw_new_surface"\n')]
        self.assertTrue(pause_mentions(titled, "zw_new_surface"))

    def test_new_surface_needs_scene_step_docs_and_core(self):
        """Same change: a new page is red until all four artifacts exist.

        Control: once scene + pause_step title + docs + CORE are present,
        green — so the lock is not 'every unknown name fails forever'.
        A word only in the Sidebar/Login body stays red.
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
        self.assertEqual(mp, [], "missing scene returns before pause/docs/core")

        ms, mp, md, mc = _collect_surface(
            f"pages/{probe}",
            probe,
            real_scenes | {probe},
            real_core,
            real_steps,
            real_doc,
        )
        self.assertEqual(ms, [])
        self.assertTrue(mp, "scene without a pause_step title is still incomplete")
        self.assertTrue(md, "scene without a docs/human-testing.md row is incomplete")
        self.assertTrue(mc, "user-visible scene must be in CORE, not extra-only")

        body_only: list[tuple[str, str]] = []
        for title, blob in real_steps:
            if title.startswith("Sidebar") or title.startswith("Login"):
                body_only.append((title, f"{blob}\n    {probe}\n"))
            else:
                body_only.append((title, blob))
        self.assertFalse(
            pause_mentions(body_only, probe),
            "a word in Sidebar/Login body is not a pause_step title",
        )
        ready_doc = real_doc + f"\n`{probe}`\n"
        ms, mp, md, mc = _collect_surface(
            f"pages/{probe}",
            probe,
            real_scenes | {probe},
            real_core | {probe},
            body_only,
            ready_doc,
        )
        self.assertEqual(ms, [])
        self.assertTrue(
            mp,
            "probe injected only into Sidebar/Login body must stay red "
            "until a dedicated pause_step title exists",
        )
        self.assertEqual(md, [])
        self.assertEqual(mc, [])

        ready_steps = real_steps + [(probe, f'pause_step "{probe}"\n')]
        ms, mp, md, mc = _collect_surface(
            f"pages/{probe}",
            probe,
            real_scenes | {probe},
            real_core,
            ready_steps,
            ready_doc,
        )
        self.assertEqual(ms, [])
        self.assertEqual(mp, [])
        self.assertEqual(md, [])
        self.assertTrue(mc, "docs+step without CORE still miss the default record")

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

    def test_fn_is_stub_rejects_pass_only_scene(self):
        tree = ast.parse(
            "class Demo:\n"
            "    def scene_billing(self):\n"
            "        '''docs only'''\n"
            "        pass\n"
            "    def scene_ok(self):\n"
            "        self.goto('/')\n"
        )
        demo = next(n for n in tree.body if isinstance(n, ast.ClassDef))
        fns = {n.name: n for n in demo.body if isinstance(n, ast.FunctionDef)}
        self.assertTrue(_fn_is_stub(fns["scene_billing"]))
        self.assertFalse(_fn_is_stub(fns["scene_ok"]))


if __name__ == "__main__":
    unittest.main(verbosity=2)
