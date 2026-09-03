#!/usr/bin/env python3
"""Layer 2 e2e: Cloud page is four Coming soon cards (catalog:e2e-cloud-coming-soon).

Visit /runners/cloud. AWS / GCP / Azure / Fleet Cloud each show Coming soon.
The page must not grow spawn, lend, enqueue, pair, or Add worker controls.
"""
from __future__ import annotations

import json
import os
import re
import socket
import sys
import time
import unittest
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runners_e2e_helpers import UI, create_host, list_hosts, wait_dx_ready  # noqa: E402
from stack_identity import ForeignStackError, require_this_tree  # noqa: E402

API = os.environ.get("FLEET_API_URL", "http://127.0.0.1:8080").rstrip("/")
REPO = Path(__file__).resolve().parents[2]
BROWSERS = os.environ.get(
    "PLAYWRIGHT_BROWSERS_PATH",
    str(REPO / ".tmp" / "ms-playwright"),
)
os.environ.setdefault("PLAYWRIGHT_BROWSERS_PATH", BROWSERS)

CLOUD_NAMES = ("AWS", "GCP", "Azure", "Fleet Cloud")
_ACTION_RE = re.compile(r"spawn|lend|enqueue|add worker|\bpair\b", re.I)
_REMOTE_ENQUEUE_RE = re.compile(
    r"/(?:spawn|enqueue|lease|tick|fleet/runs|worker-hosts|jobs|claims|lend)(?:/|$|\?)",
    re.I,
)


def _listening(host: str, port: int) -> bool:
    try:
        with socket.create_connection((host, port), timeout=2):
            return True
    except OSError:
        return False


def _require_stack() -> None:
    try:
        require_this_tree(REPO, API)
    except ForeignStackError as e:
        raise RuntimeError(str(e)) from e
    if not _listening("127.0.0.1", 3030):
        raise RuntimeError(
            "UI :3030 is not listening. Start this tree: ./scripts/human-test.sh launch"
        )


def _require_playwright():
    try:
        from playwright.sync_api import sync_playwright
    except ImportError as e:
        raise RuntimeError(
            "playwright is not installed. Run "
            "crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh "
            "with ENABLE_BROWSER_E2E=1 (also installs Chromium OS libraries)."
        ) from e
    return sync_playwright


def _signup_api(page) -> tuple[str, str]:
    """Cookie session via API so a stuck dx rebuild banner cannot hang signup."""
    email = f"catalog-cloud-{uuid.uuid4().hex[:12]}@example.test"
    password = "e2e-pass-ok"
    headers = {"Content-Type": "application/json"}
    created = None
    last = ""
    for _ in range(12):
        created = page.request.post(
            f"{UI}/api/auth/signup",
            data=json.dumps(
                {
                    "email": email,
                    "password": password,
                    "display_name": "E2E catalog-cloud",
                }
            ),
            headers=headers,
        )
        if created.status in {200, 201}:
            break
        last = f"signup {created.status}: {created.text()[:400]}"
        time.sleep(0.75)
    if created is None or created.status not in {200, 201}:
        raise RuntimeError(last or "signup failed")
    logged = page.request.post(
        f"{UI}/api/auth/login",
        data=json.dumps({"email": email, "password": password}),
        headers=headers,
    )
    if not logged.ok:
        raise RuntimeError(f"login {logged.status}: {logged.text()[:400]}")
    body = logged.json()
    ws = str(((body.get("default_workspace") or {}).get("id")) or "")
    if not ws:
        me = page.request.get(f"{UI}/api/auth/me")
        spaces = (me.json() or {}).get("workspaces") or []
        ws = str((spaces[0].get("id") if spaces else "") or "")
    if not ws:
        raise RuntimeError(f"login missing workspace: {body!r}"[:400])
    return ws, f"{UI}/{ws}"


def _goto_cloud(page, base: str) -> None:
    last_err: Exception | None = None
    for _ in range(24):
        try:
            page.goto(
                f"{base}/runners/cloud",
                wait_until="domcontentloaded",
                timeout=15_000,
            )
            wait_dx_ready(page)
            page.locator("h1").wait_for(state="visible", timeout=8_000)
            if not re.search(r"/[0-9a-f-]{36}/runners/cloud", page.url):
                raise RuntimeError(f"not on cloud: {page.url}")
            last_err = None
            break
        except Exception as e:  # noqa: BLE001 — dx mid-rebuild; retry
            last_err = e
            time.sleep(2)
    if last_err is not None:
        raise last_err


def _is_remote_enqueue_or_spawn(method: str, url: str) -> bool:
    if method.upper() not in {"POST", "PUT", "PATCH", "DELETE"}:
        return False
    if "lend" in url.lower():
        return True
    return _REMOTE_ENQUEUE_RE.search(url) is not None


def _own_kind_hosts(payload: dict, workspace_id: str) -> list[dict]:
    """Workspace-owned kind=host rows. Plane / foreign cluster cards are not this picker."""
    rows = payload.get("hosts") if isinstance(payload, dict) else payload
    if not isinstance(rows, list):
        rows = []
    own: list[dict] = []
    for host in rows:
        if not isinstance(host, dict):
            continue
        if str(host.get("kind") or "") != "host":
            continue
        ws = host.get("workspace_id")
        if ws is not None and str(ws) != workspace_id:
            continue
        own.append(host)
    return own


def _looks_like_cloud_host(host: dict) -> bool:
    name = str(host.get("name") or host.get("hostname") or "").strip()
    kind = str(host.get("kind") or "").strip().lower().replace("_", "-")
    isolation = str(host.get("isolation") or "").strip().lower().replace("_", "-")
    if name in CLOUD_NAMES:
        return True
    cloud_slugs = {
        "cloud",
        "aws",
        "gcp",
        "azure",
        "fleet-cloud",
        "public-cloud",
        "partners",
    }
    return kind in cloud_slugs or isolation in cloud_slugs


class CatalogCloudComingSoonE2ETests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        _require_stack()
        sync_playwright = _require_playwright()
        cls._pw = sync_playwright().start()
        try:
            cls._browser = cls._pw.chromium.launch(headless=True)
        except Exception as e:
            cls._pw.stop()
            raise RuntimeError(
                f"Chromium launch failed ({e}). Need OS libraries "
                "(nss, atk, gtk, gbm, asound, …) and a browser under "
                ".tmp/ms-playwright. Re-run install-full-suite-deps.sh "
                "with ENABLE_BROWSER_E2E=1 in repo-root .env."
            ) from e
        cls._context = cls._browser.new_context(
            base_url=UI,
            viewport={"width": 1400, "height": 900},
        )
        cls.page = cls._context.new_page()
        cls.page.set_default_timeout(20_000)

    @classmethod
    def tearDownClass(cls) -> None:
        cls._context.close()
        cls._browser.close()
        cls._pw.stop()

    def test_cloud_page_coming_soon(self) -> None:
        page = self.page
        _ws, base = _signup_api(page)

        mutating: list[tuple[str, str]] = []

        def on_request(req) -> None:
            method = req.method.upper()
            if method in {"POST", "PUT", "PATCH", "DELETE"}:
                mutating.append((method, req.url))

        page.on("request", on_request)
        try:
            _goto_cloud(page, base)
            self.assertRegex(page.url, r"/[0-9a-f-]{36}/runners/cloud(?:[?#].*)?$")
            h1 = page.locator("h1").first.inner_text().strip()
            self.assertIn("Cloud", h1, f"unexpected cloud heading {h1!r}")

            main = page.locator("main")
            cards = main.locator("article")
            self.assertEqual(
                cards.count(),
                4,
                "cloud page must render four Coming soon cards",
            )
            for name in CLOUD_NAMES:
                card = cards.filter(has_text=name)
                self.assertEqual(card.count(), 1, f"expected one {name} card")
                text = card.first.inner_text()
                self.assertIn(name, text)
                self.assertIn(
                    "coming soon",
                    text.lower(),
                    f"{name} must show Coming soon",
                )
            self.assertEqual(
                main.get_by_text("Coming soon", exact=True).count(),
                4,
                "each of the four cloud cards must show Coming soon",
            )

            self.assertEqual(
                main.get_by_role("button").count(),
                0,
                "cloud page must not render any buttons",
            )
            for label in ("Spawn", "Lend", "Add worker", "Enqueue", "Pair"):
                self.assertEqual(
                    page.get_by_role("button", name=label).count(),
                    0,
                    f"cloud page must not expose {label!r}",
                )
            self.assertEqual(
                page.get_by_role("button", name=re.compile(r"add worker", re.I)).count(),
                0,
                "cloud page must not pair workers",
            )
            body = main.inner_text().lower()
            for needle in ("spawn", "lend", "enqueue", "add worker"):
                self.assertNotIn(
                    needle,
                    body,
                    f"cloud page must not offer {needle}",
                )
            for i in range(main.get_by_role("link").count()):
                link = main.get_by_role("link").nth(i)
                if not link.is_visible():
                    continue
                name = " ".join((link.inner_text() or "").split())
                self.assertIsNone(
                    _ACTION_RE.search(name),
                    f"cloud page must not link to {name!r}",
                )
        finally:
            page.remove_listener("request", on_request)

        forbidden = [
            (method, url)
            for method, url in mutating
            if _is_remote_enqueue_or_spawn(method, url)
        ]
        self.assertEqual(
            forbidden,
            [],
            "cloud page must not enqueue, spawn, or lend via API",
        )

    def test_cloud_four_provider_cards_picker_does_not_create_hosts(self) -> None:
        """catalog:list-cloud-four-provider-cards

        AWS / GCP / Azure / Fleet Cloud are Coming soon articles. There is no
        enqueue/spawn/lend control. Clicking a card (the provider picker) must
        not POST a worker-host. A silent native create is a fail.
        """
        page = self.page
        ws, base = _signup_api(page)
        before = _own_kind_hosts(list_hosts(page, ws), ws)
        self.assertEqual(
            before,
            [],
            "fresh workspace must have no kind=host rows before the Cloud visit",
        )

        mutating: list[tuple[str, str]] = []

        def on_request(req) -> None:
            method = req.method.upper()
            if method in {"POST", "PUT", "PATCH", "DELETE"}:
                mutating.append((method, req.url))

        page.on("request", on_request)
        try:
            _goto_cloud(page, base)
            self.assertRegex(page.url, r"/[0-9a-f-]{36}/runners/cloud(?:[?#].*)?$")
            h1 = page.locator("h1").first.inner_text().strip()
            self.assertEqual(h1, "Cloud", f"unexpected cloud heading {h1!r}")

            main = page.locator("main")
            cards = main.locator("article")
            self.assertEqual(
                cards.count(),
                4,
                "cloud page must render four Coming soon cards",
            )
            for i, name in enumerate(CLOUD_NAMES):
                text = cards.nth(i).inner_text()
                self.assertIn(name, text, f"card {i} must be {name}")
                self.assertIn(
                    "coming soon",
                    text.lower(),
                    f"{name} must show Coming soon",
                )
            self.assertEqual(
                main.get_by_text("Coming soon", exact=True).count(),
                4,
                "each of the four cloud cards must show Coming soon",
            )

            self.assertEqual(
                main.get_by_role("button").count(),
                0,
                "cloud page must not render enqueue/spawn/lend buttons",
            )
            self.assertEqual(
                main.locator("select").count(),
                0,
                "cloud page must not render a provider <select> picker",
            )
            self.assertEqual(
                main.get_by_role("combobox").count(),
                0,
                "cloud page must not render a provider combobox picker",
            )
            for label in ("Spawn", "Lend", "Add worker", "Enqueue", "Pair"):
                self.assertEqual(
                    page.get_by_role("button", name=label).count(),
                    0,
                    f"cloud page must not expose {label!r}",
                )
            body = main.inner_text().lower()
            for needle in ("spawn", "lend", "enqueue", "add worker"):
                self.assertNotIn(
                    needle,
                    body,
                    f"cloud page must not offer {needle}",
                )

            # Click each provider card. A click-to-create picker is the hole
            # the visit-only sibling does not cover.
            for i, name in enumerate(CLOUD_NAMES):
                cards.nth(i).click(force=True)
                page.wait_for_timeout(250)
                self.assertRegex(
                    page.url,
                    r"/[0-9a-f-]{36}/runners/cloud(?:[?#].*)?$",
                    f"clicking {name} must stay on Cloud, not a create-host flow: {page.url}",
                )
                still = page.locator("main").locator("article")
                self.assertEqual(
                    still.count(),
                    4,
                    f"clicking {name} must not replace the four Coming soon cards",
                )
                self.assertIn(
                    "coming soon",
                    still.nth(i).inner_text().lower(),
                    f"{name} must stay Coming soon after click",
                )
        finally:
            page.remove_listener("request", on_request)

        forbidden = [
            (method, url)
            for method, url in mutating
            if _is_remote_enqueue_or_spawn(method, url)
        ]
        self.assertEqual(
            forbidden,
            [],
            "clicking Cloud provider cards must not enqueue, spawn, lend, or POST worker-hosts",
        )

        after = _own_kind_hosts(list_hosts(page, ws), ws)
        self.assertEqual(
            after,
            [],
            "Cloud picker must not create a worker-hosts row "
            f"(including a silent native host): {after!r}",
        )
        listed = list_hosts(page, ws)
        for host in listed.get("hosts") or []:
            if not isinstance(host, dict):
                continue
            if str(host.get("workspace_id") or "") not in {"", ws}:
                continue
            self.assertFalse(
                _looks_like_cloud_host(host),
                f"Cloud picker must not present a cloud host: {host!r}",
            )

        # Control: omitted-kind create still 200 native so an empty list is not
        # a tautology (API up, this workspace can own a host).
        created = create_host(page, ws, "ok")
        host = created.get("host") if isinstance(created, dict) else None
        self.assertIsInstance(host, dict, f"control create must return a host: {created!r}")
        self.assertEqual(
            str(host.get("isolation") or ""),
            "native",
            f"control create must stay native: {created!r}",
        )
        self.assertEqual(
            str(host.get("kind") or ""),
            "host",
            f"control create is a self-hosted host: {created!r}",
        )
        control_id = str(host.get("id") or "")
        self.assertTrue(control_id, f"control create missing id: {created!r}")
        own = _own_kind_hosts(list_hosts(page, ws), ws)
        own_ids = {str(h.get("id") or "") for h in own}
        self.assertIn(
            control_id,
            own_ids,
            "control POST without kind must appear in GET worker-hosts",
        )
        self.assertEqual(
            len(own),
            1,
            f"only the control native host may exist after the Cloud visit: {own!r}",
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
