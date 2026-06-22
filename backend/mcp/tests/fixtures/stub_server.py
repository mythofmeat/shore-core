#!/usr/bin/env python3
"""Minimal MCP stdio server for shore-mcp integration tests.

Speaks newline-delimited JSON-RPC 2.0 over stdio: handles `initialize`,
`tools/list`, `tools/call` (one `echo` tool), and `ping`. Not a real server —
just enough surface to exercise the client end to end.
"""
import json
import sys


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        msg = json.loads(line)
        method = msg.get("method")
        mid = msg.get("id")

        if method == "initialize":
            send({
                "jsonrpc": "2.0",
                "id": mid,
                "result": {
                    "protocolVersion": msg["params"]["protocolVersion"],
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "stub", "version": "0.0.1"},
                },
            })
        elif method == "notifications/initialized":
            pass  # notification: no response
        elif method == "ping":
            send({"jsonrpc": "2.0", "id": mid, "result": {}})
        elif method == "tools/list":
            send({
                "jsonrpc": "2.0",
                "id": mid,
                "result": {
                    "tools": [
                        {
                            "name": "echo",
                            "description": "Echo back the message.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {"message": {"type": "string"}},
                                "required": ["message"],
                            },
                        }
                    ]
                },
            })
        elif method == "tools/call":
            params = msg.get("params", {})
            name = params.get("name")
            args = params.get("arguments") or {}
            if name == "echo":
                send({
                    "jsonrpc": "2.0",
                    "id": mid,
                    "result": {
                        "content": [
                            {"type": "text", "text": "echo: " + str(args.get("message", ""))}
                        ],
                        "isError": False,
                    },
                })
            else:
                send({
                    "jsonrpc": "2.0",
                    "id": mid,
                    "result": {
                        "content": [{"type": "text", "text": "unknown tool: " + str(name)}],
                        "isError": True,
                    },
                })
        elif mid is not None:
            send({
                "jsonrpc": "2.0",
                "id": mid,
                "error": {"code": -32601, "message": "method not found: " + str(method)},
            })


if __name__ == "__main__":
    main()
