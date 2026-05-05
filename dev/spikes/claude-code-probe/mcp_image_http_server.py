#!/usr/bin/env python3
"""Minimal MCP HTTP server that returns one image tool result.

Used to probe whether Claude Code accepts MCP image content in a
tools/call response without enabling its built-in filesystem Read tool.
"""

from __future__ import annotations

import base64
import json
import os
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any


PORT = int(os.environ.get("MCP_HTTP_PORT", "9997"))
LOG_PATH = os.environ.get("MCP_HTTP_LOG", "/tmp/mcp-image-http.log")
PNG_B64 = (
    "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAAb0lEQVR4nO3PAQkAAAyEwO9"
    "feoshgnABdLep8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3I8QUNyPEF"
    "DcjxBQ3I8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3IPanc8OLDQitxA"
    "AAAAElFTkSuQmCC"
)


def log(record: dict[str, Any]) -> None:
    try:
        with open(LOG_PATH, "a", encoding="utf-8") as f:
            f.write(json.dumps(record) + "\n")
    except OSError:
        pass


def handle_rpc(req: dict[str, Any]) -> dict[str, Any] | None:
    method = req.get("method")
    req_id = req.get("id")
    params = req.get("params") or {}
    log({"method": method, "id": req_id, "params": params})

    def ok(result: Any) -> dict[str, Any]:
        return {"jsonrpc": "2.0", "id": req_id, "result": result}

    def err(code: int, message: str) -> dict[str, Any]:
        return {"jsonrpc": "2.0", "id": req_id, "error": {"code": code, "message": message}}

    if method == "initialize":
        return ok(
            {
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "shore-image-spike", "version": "0.0.1"},
            }
        )
    if method == "notifications/initialized":
        return None
    if method == "tools/list":
        return ok(
            {
                "tools": [
                    {
                        "name": "show_image",
                        "description": "Return the attached image. The image is a solid red PNG.",
                        "inputSchema": {"type": "object", "properties": {}},
                    }
                ]
            }
        )
    if method == "tools/call":
        name = params.get("name")
        if name == "show_image":
            return ok(
                {
                    "content": [
                        {
                            "type": "image",
                            "data": PNG_B64,
                            "mimeType": "image/png",
                        }
                    ],
                    "isError": False,
                }
            )
        return err(-32601, f"Unknown tool: {name}")
    if req_id is not None:
        return err(-32601, f"Method not found: {method}")
    return None


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt: str, *args: Any) -> None:
        pass

    def do_POST(self) -> None:
        if self.path != "/mcp":
            self.send_error(404, "not /mcp")
            return
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length).decode("utf-8") if length else ""
        try:
            req = json.loads(body)
        except json.JSONDecodeError as e:
            log({"event": "bad-json", "error": str(e), "raw": body[:500]})
            self.send_error(400, f"bad json: {e}")
            return

        if isinstance(req, dict):
            resp = handle_rpc(req)
            payload = json.dumps(resp).encode("utf-8") if resp is not None else b""
            self.send_response(200 if resp is not None else 202)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            if payload:
                self.wfile.write(payload)
        else:
            self.send_error(400, "expected JSON object")


def main() -> int:
    open(LOG_PATH, "w").close()
    log({"event": "start", "port": PORT, "pid": os.getpid()})
    srv = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    print(f"HTTP MCP image server listening on 127.0.0.1:{PORT}/mcp", flush=True)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
