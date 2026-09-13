"""Live e2e for Slack / Notion / GitHub on the running plane.

Cases (this file + `crates/fleet-server/tests/integrations_live.rs`):

Inbox
  I2  POST poll errors[] / GET monitor.last_error has no slack/notion not_connected

Tool + LLM loop (Rust, live tokens + Grok CLI auth):
  T1–T6  list/get/query slack/notion/github
  L1     grok.chat_with_tools asks Slack channels and must call integration_query

Requires a running fleet-server on :8080, DATABASE_URL, JWT_SECRET,
and the connected workspace. Run:

  FLEET_LIVE_INTEGRATIONS=1 python3 tests/browser/test_integrations_live.py
  FLEET_LIVE_INTEGRATIONS=1 cargo test -p fleet-server --test integrations_live -- --ignored --nocapture
"""

from __future__ import annotations

import base64
import hashlib
import hmac
import json
import os
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
API = os.environ.get("FLEET_API_URL", "http://127.0.0.1:8080").rstrip("/")
WS = "01a03929-e0ad-7dd3-9835-71c9c4f5df01"


def _load_env() -> dict[str, str]:
    out: dict[str, str] = {}
    p = ROOT / ".env"
    if not p.is_file():
        return out
    for line in p.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        k, v = line.split("=", 1)
        out[k] = v
    return out


def _b64url(raw: bytes) -> str:
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode("ascii")


def _issue_jwt(secret: str, user_id: str, email: str, sid: str) -> str:
    header = _b64url(json.dumps({"alg": "HS256", "typ": "JWT"}, separators=(",", ":")).encode())
    now = int(time.time())
    payload = _b64url(
        json.dumps(
            {
                "sub": user_id,
                "email": email,
                "sid": sid,
                "iat": now,
                "exp": now + 900,
            },
            separators=(",", ":"),
        ).encode()
    )
    sig = hmac.new(secret.encode(), f"{header}.{payload}".encode(), hashlib.sha256).digest()
    return f"{header}.{payload}.{_b64url(sig)}"


def _sql(sql: str) -> list[list[str]]:
    import subprocess

    url = os.environ.get(
        "DATABASE_URL",
        "postgres://fleet:fleet@127.0.0.1:5433/fleet?sslmode=disable",
    )
    out = subprocess.check_output(["psql", url, "-At", "-F", "\t", "-c", sql], text=True)
    rows = []
    for line in out.splitlines():
        if line:
            rows.append(line.split("\t"))
    return rows


def _request(method: str, path: str, token: str, body: dict | None = None) -> tuple[int, dict | str]:
    data = None
    headers = {"Cookie": f"fleet_access={token}", "Accept": "application/json"}
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(API + path, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            raw = r.read().decode()
            try:
                return r.status, json.loads(raw) if raw else {}
            except json.JSONDecodeError:
                return r.status, raw
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            return e.code, json.loads(raw) if raw else {"error": raw}
        except json.JSONDecodeError:
            return e.code, raw


def _ok(name: str, cond: bool, detail: str = "") -> None:
    mark = "PASS" if cond else "FAIL"
    print(f"  [{mark}] {name}" + (f" — {detail}" if detail else ""))
    if not cond:
        raise AssertionError(f"{name}: {detail}")


def _slack_or_notion_not_connected(blob: str) -> str:
    """Provider named as not_connected. Ignores a C1-allowed dead GitHub."""
    low = blob.lower()
    for provider in ("slack", "notion"):
        if f"{provider} is not connected" in low or f"{provider} credentials missing" in low:
            return provider
    return ""


def main() -> int:
    if os.environ.get("FLEET_LIVE_INTEGRATIONS") != "1":
        print("set FLEET_LIVE_INTEGRATIONS=1 to run live integration e2e")
        return 2
    env = _load_env()
    secret = env.get("JWT_SECRET", "").strip()
    if not secret:
        print("JWT_SECRET missing in .env")
        return 2

    health = urllib.request.urlopen(API + "/health", timeout=5)
    h = json.loads(health.read().decode())
    print(f"API {API} git_sha={h.get('git_sha')} apis={h.get('apis')}")

    ws = os.environ.get("FLEET_LIVE_WORKSPACE_ID", WS)
    who = _sql(
        "select u.id::text, u.email, s.id::text from workspace_members m "
        "join users u on u.id=m.user_id "
        "join auth_sessions s on s.user_id=u.id "
        f"where m.workspace_id='{ws}' and s.revoked_at is null "
        "and s.expires_at > now() order by s.created_at desc limit 1"
    )
    if not who:
        print("no active session for connected workspace — log in to the UI first")
        return 2
    user_id, email, sid = who[0]
    token = _issue_jwt(secret, user_id, email, sid)
    failed = 0

    def case(name: str, cond: bool, detail: str = "") -> None:
        nonlocal failed
        try:
            _ok(name, cond, detail)
        except AssertionError as e:
            failed += 1
            print(f"  [FAIL] {e}")

    print("\n== Inbox ==")
    st, polled = _request("POST", f"/api/workspaces/{ws}/inbox/poll", token, {})
    errors = polled.get("errors") if isinstance(polled, dict) else None
    case(
        "I2 poll errors list",
        isinstance(errors, list),
        f"{st} {polled}"[:200],
    )
    err_hit = ""
    if isinstance(errors, list):
        for item in errors:
            if _slack_or_notion_not_connected(str(item)):
                err_hit = str(item)
                break
    case(
        "I2 poll not_connected",
        not err_hit,
        err_hit[:160],
    )
    st, inbox = _request("GET", f"/api/workspaces/{ws}/inbox", token)
    last_error = ""
    if isinstance(inbox, dict) and isinstance(inbox.get("monitor"), dict):
        last_error = str(inbox["monitor"].get("last_error") or "")
    case(
        "I2 monitor last_error not slack/notion not_connected",
        not _slack_or_notion_not_connected(last_error),
        last_error[:160],
    )

    print("\n== summary ==")
    if failed:
        print(f"{failed} failed")
        return 1
    print("all HTTP cases passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
