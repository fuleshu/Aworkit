#!/usr/bin/env python3
"""End-to-end check for the ComfyUI bridge plugin.

It starts a small HTTP server that answers the ComfyUI API endpoints the bridge
uses, drives the bridge over real MCP stdio framing, and asserts every tool
result. It is deliberately dependency-free so it runs anywhere Python does:

    python3 test_bridge.py
"""

from __future__ import annotations

import json
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

HERE = Path(__file__).resolve().parent
BRIDGE = HERE / "bridge.py"
WORKFLOWS = HERE / "workflows.json"


class ComfyHandler(BaseHTTPRequestHandler):
    def log_message(self, *args):  # Keep the test output readable.
        pass

    def _send(self, payload: dict, status: int = 200) -> None:
        body = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):  # noqa: N802 - required by BaseHTTPRequestHandler
        if self.path.startswith("/system_stats"):
            self._send({"system": {"comfyui_version": "test"}, "devices": []})
        elif self.path.startswith("/history/"):
            self._send(
                {
                    "prompt-id-1": {
                        "outputs": {
                            "9": {
                                "images": [
                                    {
                                        "filename": "aworkit_00001_.png",
                                        "subfolder": "",
                                        "type": "output",
                                    }
                                ]
                            }
                        }
                    }
                }
            )
        else:
            self._send({"error": "not found"}, status=404)

    def do_POST(self):  # noqa: N802 - required by BaseHTTPRequestHandler
        length = int(self.headers.get("Content-Length", "0"))
        body = json.loads(self.rfile.read(length) or b"{}")
        if self.path.startswith("/prompt"):
            # Prove the bridge really sent an API workflow graph.
            if "3" not in body.get("prompt", {}):
                self._send({"error": "missing graph"}, status=400)
                return
            self._send({"prompt_id": "prompt-id-1", "number": 1})
        else:
            self._send({"error": "not found"}, status=404)


def start_server() -> tuple[ThreadingHTTPServer, int]:
    server = ThreadingHTTPServer(("127.0.0.1", 0), ComfyHandler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server, server.server_address[1]


def call(bridge: subprocess.Popen, message: dict) -> dict:
    assert bridge.stdin is not None and bridge.stdout is not None
    bridge.stdin.write(json.dumps(message) + "\n")
    bridge.stdin.flush()
    line = bridge.stdout.readline()
    if not line:
        raise AssertionError("bridge closed its output early")
    return json.loads(line)


def main() -> int:
    server, port = start_server()
    endpoint = f"http://127.0.0.1:{port}"
    bridge = subprocess.Popen(
        [sys.executable, str(BRIDGE), "--endpoint", endpoint, "--workflows", str(WORKFLOWS)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        discover = call(
            bridge,
            {"jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {}},
        )
        assert "error" in discover, discover
        assert discover["error"]["code"] == -32601, discover

        initialized = call(
            bridge,
            {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "test", "version": "1"},
                },
            },
        )
        assert initialized["result"]["protocolVersion"] == "2024-11-05", initialized
        assert "tools" in initialized["result"]["capabilities"], initialized

        catalog = call(bridge, {"jsonrpc": "2.0", "id": 3, "method": "tools/list", "params": {}})
        names = [tool["name"] for tool in catalog["result"]["tools"]]
        assert names == [
            "comfyui_status",
            "comfyui_list_workflows",
            "comfyui_run_workflow",
        ], names

        status = call(
            bridge,
            {
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {"name": "comfyui_status", "arguments": {}},
            },
        )
        assert status["result"]["structuredContent"]["reachable"] is True, status
        assert status["result"]["isError"] is False, status

        queued = call(
            bridge,
            {
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": {
                    "name": "comfyui_run_workflow",
                    "arguments": {
                        "workflow": "example_text_to_image",
                        "inputs": {"6.text": "a red fox in snow"},
                        "timeout_seconds": 10,
                    },
                },
            },
        )
        structured = queued["result"]["structuredContent"]
        assert queued["result"]["isError"] is False, queued
        assert structured["status"] == "completed", structured
        assert structured["prompt_id"] == "prompt-id-1", structured
        assert structured["images"][0]["filename"] == "aworkit_00001_.png", structured
        assert "/view?filename=aworkit_00001_.png" in structured["images"][0]["url"], structured

        unknown = call(
            bridge,
            {
                "jsonrpc": "2.0",
                "id": 6,
                "method": "tools/call",
                "params": {"name": "comfyui_run_workflow", "arguments": {"workflow": "missing"}},
            },
        )
        assert unknown["result"]["isError"] is True, unknown
        assert "Unknown workflow" in unknown["result"]["content"][0]["text"], unknown

        # Degraded: an unreachable endpoint is an explicit error result, not a crash.
        offline = subprocess.Popen(
            [
                sys.executable,
                str(BRIDGE),
                "--endpoint",
                "http://127.0.0.1:1",
                "--workflows",
                str(WORKFLOWS),
            ],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            call(
                offline,
                {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {"protocolVersion": "2024-11-05", "capabilities": {}},
                },
            )
            degraded = call(
                offline,
                {
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {"name": "comfyui_status", "arguments": {}},
                },
            )
            assert degraded["result"]["isError"] is True, degraded
            assert "not reachable" in degraded["result"]["content"][0]["text"], degraded
        finally:
            offline.stdin.close()
            offline.terminate()
            offline.wait(timeout=10)

        print("comfyui-bridge: all checks passed")
        return 0
    finally:
        server.shutdown()
        if bridge.poll() is None:
            bridge.stdin.close()
            bridge.terminate()
            bridge.wait(timeout=10)


if __name__ == "__main__":
    raise SystemExit(main())
