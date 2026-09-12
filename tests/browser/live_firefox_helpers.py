#!/usr/bin/env python3
"""Firefox-cookie session against the live Agent Fleet instance.

Used by `test_live_firefox_session.py`. Does not pair, rotate tokens, start
the 2-task fleet fixture, or enable Inbox monitor (that would pick up real
boards). Direct run:

  python3 tests/browser/test_live_firefox_session.py -v
"""
from __future__ import annotations

import base64
import hashlib
import hmac
import json
import os
import shutil
import sqlite3
import subprocess
import tempfile
import time
import uuid
from pathlib import Path
from typing import Any

BASE = os.environ.get("FLEET_LIVE_URL", "https://agentfleet.studio").rstrip("/")
PROF = Path(
    os.environ.get(
        "FLEET_FIREFOX_PROFILE",
        str(Path.home() / ".config/mozilla/firefox/9g6h0a6w.default-release"),
    )
)
UA = (
    "Mozilla/5.0 (X11; Linux x86_64; rv:155.0) Gecko/20100101 Firefox/155.0"
)
VINY_EMAIL = "viniciusbrun.dev@gmail.com"
SECRET_MARKERS = ("pair_token", "machine_token", "flm_", "flp_", "fleet_access")


def _profile_cookies() -> list[dict]:
    if not PROF.is_dir():
        raise RuntimeError(f"Firefox profile missing: {PROF}")
    td = Path(tempfile.mkdtemp(prefix="ffcook-"))
    try:
        for n in ("cookies.sqlite", "cookies.sqlite-wal", "cookies.sqlite-shm"):
            src = PROF / n
            if src.exists():
                shutil.copy2(src, td / n)
        db = td / "cookies.sqlite"
        if not db.is_file():
            raise RuntimeError(f"no cookies.sqlite in {PROF}")
        con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        out = []
        for host, name, value, path, expiry, secure, httponly in con.execute(
            """
            SELECT host, name, value, path, expiry, isSecure, isHttpOnly
            FROM moz_cookies
            WHERE host = 'agentfleet.studio'
            """
        ):
            out.append(
                {
                    "name": name,
                    "value": value,
                    "domain": host,
                    "path": path or "/",
                    "expires": int(expiry) if expiry and expiry < 4_000_000_000 else -1,
                    "httpOnly": bool(httponly),
                    "secure": bool(secure),
                }
            )
        con.close()
    finally:
        shutil.rmtree(td, ignore_errors=True)
    return out


def firefox_cookies() -> list[dict]:
    """Prefer the live Firefox jar. If that refresh family is revoked, mint
    a new session for the same user (does not write back into Firefox)."""
    out = _profile_cookies()
    names = {c["name"] for c in out}
    if "fleet_access" in names and "fleet_refresh" in names:
        return out
    raise RuntimeError(
        f"Firefox profile has no live session cookies (have {sorted(names)})"
    )


def _b64url(raw: bytes) -> str:
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode("ascii")


def _issue_access(secret: str, user_id: str, email: str, sid: str) -> str:
    header = _b64url(json.dumps({"alg": "HS256", "typ": "JWT"}, separators=(",", ":")).encode())
    now = int(time.time())
    payload = _b64url(
        json.dumps(
            {
                "sub": user_id,
                "email": email,
                "sid": sid,
                "iat": now,
                "exp": now + 8 * 3600,
            },
            separators=(",", ":"),
        ).encode()
    )
    sig = hmac.new(secret.encode(), f"{header}.{payload}".encode(), hashlib.sha256).digest()
    return f"{header}.{payload}.{_b64url(sig)}"


def mint_viny_session_cookies() -> list[dict]:
    """Issue a new live session for VINY_EMAIL after the Firefox jar is stale.

    Reads JWT_SECRET on the VPS only inside the remote shell. Tokens are not
    printed. Does not rotate machine tokens or pair tokens.
    """
    remote = r"""
set -euo pipefail
secret=$(grep -E '^JWT_SECRET=' /home/agent-fleet/stack/.env | cut -d= -f2-)
uid=$(docker exec agent-fleet-postgres-1 psql -U fleet -d fleet -tAc \
  "SELECT id FROM users WHERE email_normalized = lower(trim('viniciusbrun.dev@gmail.com'));")
printf '%s\n%s\n' "$secret" "$uid"
"""
    raw = subprocess.check_output(
        [
            "ssh",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=8",
            "-i",
            str(Path.home() / ".ssh/agent-fleet"),
            "agent-fleet@82.39.213.33",
            remote,
        ],
        text=True,
    )
    lines = [ln.strip() for ln in raw.splitlines() if ln.strip()]
    if len(lines) < 2:
        raise RuntimeError("could not mint live session (missing secret or user id)")
    secret, user_id = lines[0], lines[1]
    sid = str(uuid.uuid4())
    family = str(uuid.uuid4())
    refresh = uuid.uuid4().hex + uuid.uuid4().hex
    digest = hashlib.sha256(refresh.encode()).hexdigest()
    access = _issue_access(secret, user_id, VINY_EMAIL, sid)
    insert = (
        "INSERT INTO auth_sessions "
        "(id, user_id, refresh_token_hash, family_id, user_agent, expires_at, absolute_expires_at, last_seen_at) "
        f"VALUES ('{sid}', '{user_id}', decode('{digest}','hex'), '{family}', "
        "'live-firefox-sim', now() + interval '7 days', now() + interval '30 days', now());"
    )
    subprocess.check_call(
        [
            "ssh",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=8",
            "-i",
            str(Path.home() / ".ssh/agent-fleet"),
            "agent-fleet@82.39.213.33",
            f"docker exec -i agent-fleet-postgres-1 psql -U fleet -d fleet -v ON_ERROR_STOP=1 -c {json.dumps(insert)}",
        ]
    )
    expiry = int(time.time()) + 7 * 24 * 3600
    return [
        {
            "name": "fleet_access",
            "value": access,
            "domain": "agentfleet.studio",
            "path": "/",
            "expires": expiry,
            "httpOnly": True,
            "secure": True,
        },
        {
            "name": "fleet_refresh",
            "value": refresh,
            "domain": "agentfleet.studio",
            "path": "/",
            "expires": expiry,
            "httpOnly": True,
            "secure": True,
        },
    ]


def json_or_error(res) -> dict[str, Any]:
    text = res.text()
    if not res.ok:
        raise RuntimeError(f"{res.status} {res.url}: {text[:500]}")
    if not text:
        return {}
    return json.loads(text)


def assert_no_secrets(blob: str, where: str) -> None:
    lower = blob.lower()
    for marker in SECRET_MARKERS:
        if marker == "flp_" or marker == "flm_":
            if marker in blob:
                raise AssertionError(f"{where} leaked {marker}")
        elif marker in lower:
            raise AssertionError(f"{where} leaked {marker}")


def wait_h1(page, title: str, timeout: int = 20_000) -> None:
    page.locator("h1").filter(has_text=title).first.wait_for(timeout=timeout)


def goto_app(page, path: str) -> None:
    url = path if path.startswith("http") else f"{BASE}{path}"
    page.goto(url, wait_until="domcontentloaded", timeout=30_000)
    page.locator("aside").first.wait_for(timeout=20_000)


def expand_nav(page, title: str) -> None:
    btn = page.get_by_role("button", name=title, exact=True)
    if btn.count() == 0:
        return
    try:
        btn.first.click(timeout=2_000)
    except Exception:
        pass
