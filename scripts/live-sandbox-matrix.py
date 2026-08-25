#!/usr/bin/env python3
"""Live matrix: chat with local Ollama + spawn every host isolation we can.

Writes `.tmp/sandbox-matrix/<stamp>/report.json`. Uses the running this-tree
API on :8080. Throwaway *@example.test account.
"""
from __future__ import annotations

import json
import os
import sys
import time
import uuid
from datetime import datetime, timezone
from http.cookiejar import CookieJar
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import HTTPCookieProcessor, Request, build_opener

REPO = Path(__file__).resolve().parents[1]
API = os.environ.get("FLEET_API_URL", "http://127.0.0.1:8080").rstrip("/")
PASSWORD = "e2e-pass-ok"


class Api:
    def __init__(self) -> None:
        self.opener = build_opener(HTTPCookieProcessor(CookieJar()))

    def call(self, method: str, path: str, body: dict | None = None) -> tuple[int, dict | str]:
        data = None
        headers = {"Accept": "application/json"}
        if body is not None:
            data = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
        req = Request(API + path, data=data, headers=headers, method=method)
        try:
            with self.opener.open(req, timeout=180) as resp:
                raw = resp.read().decode()
                return resp.status, json.loads(raw) if raw else {}
        except HTTPError as e:
            raw = e.read().decode()
            try:
                parsed = json.loads(raw)
            except json.JSONDecodeError:
                parsed = raw
            return e.code, parsed


def main() -> int:
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    out = REPO / ".tmp" / "sandbox-matrix" / stamp
    out.mkdir(parents=True, exist_ok=True)
    report: dict = {"stamp": stamp, "steps": []}

    def rec(title: str, **kw) -> None:
        report["steps"].append({"title": title, **kw})
        print(f"==> {title}: {json.dumps({k: v for k, v in kw.items() if k != 'body'}, default=str)[:400]}", flush=True)

    api = Api()
    email = f"matrix.{uuid.uuid4().hex[:10]}@example.test"
    st, body = api.call(
        "POST",
        "/api/auth/signup",
        {"email": email, "password": PASSWORD, "display_name": "Matrix User"},
    )
    rec("signup", status=st, email=email)
    if st not in (200, 201):
        rec("fail", reason="signup", body=body)
        (out / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        return 1

    st, body = api.call("POST", "/api/auth/login", {"email": email, "password": PASSWORD})
    rec("login", status=st)
    ws = (body.get("default_workspace") or {}).get("id") if isinstance(body, dict) else None
    if not ws:
        rec("fail", reason="no workspace", body=body)
        (out / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        return 1
    report["workspace_id"] = ws

    # Ensure fleet project exists
    st, fleet = api.call("GET", f"/api/workspaces/{ws}/fleet")
    rec("fleet_project", status=st)

    st, status = api.call("GET", f"/api/workspaces/{ws}/chat/status")
    rec("chat_status", status=st, body=status if isinstance(status, dict) else str(status)[:500])

    # Multi-turn local LLM conversation
    st, turn1 = api.call(
        "POST",
        f"/api/workspaces/{ws}/chat",
        {
            "provider": "ollama",
            "model": "llama3.2:1b",
            "messages": [
                {
                    "role": "user",
                    "content": "You are in a live product check. Reply with one short sentence that includes the word SANDWICH and ask me what isolation I want next.",
                }
            ],
        },
    )
    rec("llm_turn1", status=st, body=turn1 if isinstance(turn1, dict) else str(turn1)[:800])
    cid = turn1.get("conversation_id") if isinstance(turn1, dict) else None
    reply1 = ""
    if isinstance(turn1, dict):
        reply1 = (turn1.get("message") or {}).get("content") or turn1.get("content") or ""
    ok_llm = st == 200 and "sandwich" in reply1.lower()
    rec("llm_turn1_ok", ok=ok_llm, reply=reply1[:400], conversation_id=cid)

    st, turn2 = api.call(
        "POST",
        f"/api/workspaces/{ws}/chat",
        {
            "provider": "ollama",
            "model": "llama3.2:1b",
            "conversation_id": cid,
            "messages": [
                {"role": "user", "content": reply1 or "hi"},
                {
                    "role": "user",
                    "content": "I want container isolation next. Confirm in one sentence using the word CONFIRMED.",
                },
            ],
        },
    )
    reply2 = ""
    if isinstance(turn2, dict):
        reply2 = (turn2.get("message") or {}).get("content") or ""
    rec(
        "llm_turn2",
        status=st,
        ok=st == 200 and "confirm" in reply2.lower(),
        reply=reply2[:400],
    )

    # Isolation process → native spawn
    st, _ = api.call(
        "PATCH",
        f"/api/workspaces/{ws}/fleet/settings",
        {"min_isolation": "process"},
    )
    rec("set_isolation_process", status=st)

    st, fx = api.call("POST", f"/api/workspaces/{ws}/fleet/runs/fixture", {"kind": "chain"})
    rec("fixture_native", status=st, body=fx if isinstance(fx, dict) else str(fx)[:300])
    st, tick = api.call("POST", f"/api/workspaces/{ws}/fleet/tick")
    rec("tick_native", status=st, body=tick if isinstance(tick, dict) else str(tick)[:500])

    native_sess = []
    for i in range(20):
        time.sleep(1)
        st, sess = api.call("GET", f"/api/workspaces/{ws}/fleet/sessions")
        rows = sess.get("sessions") if isinstance(sess, dict) else []
        native_sess = [s for s in rows if s.get("runtime_backend") == "remote"]
        if native_sess:
            break
        if i % 5 == 4:
            api.call("POST", f"/api/workspaces/{ws}/fleet/tick")
    rec("native_sessions", count=len(native_sess), sessions=native_sess)

    # Isolation container — host must advertise container (caller restarts fleet-host)
    st, _ = api.call(
        "PATCH",
        f"/api/workspaces/{ws}/fleet/settings",
        {"min_isolation": "container"},
    )
    rec("set_isolation_container", status=st)
    st, fx2 = api.call("POST", f"/api/workspaces/{ws}/fleet/runs/fixture", {"kind": "chain"})
    rec("fixture_container", status=st)
    st, tick2 = api.call("POST", f"/api/workspaces/{ws}/fleet/tick")
    rec("tick_container", status=st, body=tick2 if isinstance(tick2, dict) else str(tick2)[:500])

    container_sess = []
    for i in range(15):
        time.sleep(1)
        st, sess = api.call("GET", f"/api/workspaces/{ws}/fleet/sessions")
        rows = sess.get("sessions") if isinstance(sess, dict) else []
        container_sess = [
            s
            for s in rows
            if s.get("runtime_backend") == "remote"
            and s.get("id") not in {x.get("id") for x in native_sess}
        ]
        if container_sess:
            break
        if i % 5 == 4:
            api.call("POST", f"/api/workspaces/{ws}/fleet/tick")
    rec("container_sessions", count=len(container_sess), sessions=container_sess)

    # VM need — expect no assign (not implemented)
    st, _ = api.call(
        "PATCH",
        f"/api/workspaces/{ws}/fleet/settings",
        {"min_isolation": "vm"},
    )
    rec("set_isolation_vm", status=st)
    st, fx3 = api.call("POST", f"/api/workspaces/{ws}/fleet/runs/fixture", {"kind": "chain"})
    rec("fixture_vm", status=st)
    st, tick3 = api.call("POST", f"/api/workspaces/{ws}/fleet/tick")
    rec("tick_vm", status=st, body=tick3 if isinstance(tick3, dict) else str(tick3)[:400])
    st, tasks = api.call("GET", f"/api/workspaces/{ws}/fleet/tasks")
    rec("vm_tasks", body=tasks if isinstance(tasks, dict) else str(tasks)[:500])

    report["ok_llm"] = bool(ok_llm and st)
    (out / "report.json").write_text(json.dumps(report, indent=2, default=str) + "\n")
    print(f"wrote {out / 'report.json'}", flush=True)
    latest = REPO / ".tmp" / "sandbox-matrix" / "LATEST"
    latest.write_text(str(out) + "\n")
    return 0 if ok_llm else 2


if __name__ == "__main__":
    sys.exit(main())
