#!/usr/bin/env python3
"""Minimal ACP agent for integration tests (JSON-RPC over stdio)."""
from __future__ import annotations

import json
import sys


def read_msg():
    line = sys.stdin.readline()
    if not line:
        return None
    line = line.strip()
    if not line:
        return read_msg()
    return json.loads(line)


def write_msg(obj):
    sys.stdout.write(json.dumps(obj, separators=(",", ":")))
    sys.stdout.write("\n")
    sys.stdout.flush()


def main() -> int:
    while True:
        msg = read_msg()
        if msg is None:
            return 0

        method = msg.get("method")
        req_id = msg.get("id")

        # Notifications (no id) — ignore
        if req_id is None:
            continue

        if method == "initialize":
            write_msg(
                {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "protocolVersion": 1,
                        "agentCapabilities": {"loadSession": False},
                        "agentInfo": {"name": "mock-acp", "version": "0.0.1"},
                        "authMethods": [{"id": "cached_token", "name": "Cached"}],
                    },
                }
            )
        elif method == "authenticate":
            write_msg({"jsonrpc": "2.0", "id": req_id, "result": {}})
        elif method == "session/new":
            cwd = (msg.get("params") or {}).get("cwd", "")
            write_msg(
                {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "sessionId": "sess-mock-1",
                        "configOptions": [
                            {
                                "id": "model",
                                "name": "Model",
                                "category": "model",
                                "type": "select",
                                "currentValue": "mock-fast",
                                "options": [
                                    {"value": "mock-fast", "name": "Fast"},
                                    {"value": "mock-strong", "name": "Strong"},
                                ],
                            },
                            {
                                "id": "thought_level",
                                "name": "Thinking",
                                "category": "thought_level",
                                "type": "select",
                                "currentValue": "low",
                                "options": [
                                    {"value": "low", "name": "Low"},
                                    {"value": "high", "name": "High"},
                                ],
                            },
                        ],
                        "meta": {"cwd": cwd},
                    },
                }
            )
        elif method == "session/set_config_option":
            write_msg(
                {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "configOptions": [
                            {
                                "id": "model",
                                "name": "Model",
                                "category": "model",
                                "type": "select",
                                "currentValue": (msg.get("params") or {}).get(
                                    "value", "mock-fast"
                                ),
                                "options": [
                                    {"value": "mock-fast", "name": "Fast"},
                                    {"value": "mock-strong", "name": "Strong"},
                                ],
                            }
                        ]
                    },
                }
            )
        elif method == "session/prompt":
            # Stream a couple of updates, then finish
            write_msg(
                {
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": "sess-mock-1",
                        "update": {
                            "sessionUpdate": "agent_message_chunk",
                            "content": {"type": "text", "text": "hello from mock agent"},
                        },
                    },
                }
            )
            write_msg(
                {
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": "sess-mock-1",
                        "update": {
                            "sessionUpdate": "tool_call",
                            "title": "list_dir",
                        },
                    },
                }
            )
            write_msg(
                {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {"stopReason": "end_turn"},
                }
            )
        elif method in ("session/cancel", "session/close"):
            write_msg({"jsonrpc": "2.0", "id": req_id, "result": {}})
        elif method and "request_permission" in method:
            # Should not normally be requested by mock during prompt, but handle if client sends oddly
            write_msg(
                {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {"outcome": {"outcome": "selected", "optionId": "allow"}},
                }
            )
        else:
            write_msg({"jsonrpc": "2.0", "id": req_id, "result": {}})


if __name__ == "__main__":
    raise SystemExit(main())
