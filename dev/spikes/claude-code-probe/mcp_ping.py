#!/usr/bin/env python3
"""Minimal stdio MCP server. One tool, `ping`, returns `pong: <msg>`.

MCP is JSON-RPC 2.0 over stdio. We implement just enough to register
one tool and answer calls: initialize, notifications/initialized,
tools/list, tools/call. Everything else gets a method-not-found.

Logs every request received (one JSON line per request) to
$MCP_PING_LOG so the probes can verify the model actually called us.
"""

from __future__ import annotations

import json
import os
import sys
from typing import Any


LOG_PATH = os.environ.get("MCP_PING_LOG", "/tmp/mcp-ping.log")
PROTOCOL_VERSION = "2024-11-05"


def log(record: dict[str, Any]) -> None:
    try:
        with open(LOG_PATH, "a", encoding="utf-8") as f:
            f.write(json.dumps(record) + "\n")
    except OSError:
        pass


def send(msg: dict[str, Any]) -> None:
    line = json.dumps(msg)
    sys.stdout.write(line + "\n")
    sys.stdout.flush()


def reply(req_id: Any, result: Any) -> None:
    send({"jsonrpc": "2.0", "id": req_id, "result": result})


def reply_error(req_id: Any, code: int, message: str) -> None:
    send({"jsonrpc": "2.0", "id": req_id, "error": {"code": code, "message": message}})


def handle(req: dict[str, Any]) -> None:
    method = req.get("method")
    req_id = req.get("id")
    params = req.get("params") or {}
    log({"method": method, "id": req_id, "params": params})

    if method == "initialize":
        reply(
            req_id,
            {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "shore-spike-ping", "version": "0.0.1"},
            },
        )
    elif method == "notifications/initialized":
        # No response for notifications.
        pass
    elif method == "tools/list":
        reply(
            req_id,
            {
                "tools": [
                    {
                        "name": "ping",
                        "description": (
                            "Test tool for the shore Claude-Code spike. "
                            "Returns 'pong: <message>'. Call this when the user "
                            "explicitly asks you to ping or test the MCP tool."
                        ),
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "message": {
                                    "type": "string",
                                    "description": "Arbitrary string to echo back.",
                                }
                            },
                            "required": ["message"],
                        },
                    }
                ]
            },
        )
    elif method == "tools/call":
        name = params.get("name")
        args = params.get("arguments") or {}
        if name == "ping":
            msg = args.get("message", "")
            reply(
                req_id,
                {
                    "content": [
                        {"type": "text", "text": f"pong: {msg}"},
                    ],
                    "isError": False,
                },
            )
        else:
            reply_error(req_id, -32601, f"Unknown tool: {name}")
    elif req_id is not None:
        reply_error(req_id, -32601, f"Method not found: {method}")


def main() -> None:
    log({"event": "start", "pid": os.getpid()})
    for raw in sys.stdin:
        raw = raw.strip()
        if not raw:
            continue
        try:
            req = json.loads(raw)
        except json.JSONDecodeError as e:
            log({"event": "bad-json", "error": str(e), "raw": raw})
            continue
        try:
            handle(req)
        except Exception as e:  # noqa: BLE001
            log({"event": "handler-error", "error": repr(e)})
            if req.get("id") is not None:
                reply_error(req["id"], -32603, f"Internal error: {e}")
    log({"event": "stop"})


if __name__ == "__main__":
    main()
