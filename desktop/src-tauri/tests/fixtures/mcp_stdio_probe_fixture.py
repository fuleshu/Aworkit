#!/usr/bin/env python3
"""Hermetic legacy MCP server used by the desktop's real probe test."""

import json
import os
import sys


EXPECTED_TOKEN = "desktop-mcp-probe-secret"


def send(message):
    sys.stdout.write(json.dumps(message, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def response(request_id, result):
    send({"jsonrpc": "2.0", "id": request_id, "result": result})


def error(request_id, code, message):
    send(
        {
            "jsonrpc": "2.0",
            "id": request_id,
            "error": {"code": code, "message": message},
        }
    )


def main():
    credential_available = os.environ.get("AWORKIT_MCP_TEST_TOKEN") == EXPECTED_TOKEN
    for line in sys.stdin:
        if not line.strip():
            continue
        message = json.loads(line)
        method = message.get("method")
        request_id = message.get("id")
        if method == "server/discover":
            error(request_id, -32601, "legacy fixture")
        elif not credential_available and request_id is not None:
            error(request_id, -32000, "required credential was not injected")
        elif method == "initialize":
            response(
                request_id,
                {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {
                        "tools": {},
                        "resources": {},
                        "prompts": {},
                    },
                    "serverInfo": {
                        "name": "aworkit-desktop-probe-test",
                        "version": "1.0.0",
                    },
                },
            )
        elif method == "notifications/initialized":
            continue
        elif method == "tools/list":
            response(
                request_id,
                {
                    "tools": [
                        {
                            "name": "echo",
                            "description": "Echo one message",
                            "inputSchema": {
                                "type": "object",
                                "properties": {"message": {"type": "string"}},
                                "required": ["message"],
                            },
                        }
                    ]
                },
            )
        elif method == "resources/list":
            response(
                request_id,
                {
                    "resources": [
                        {
                            "uri": "fixture://desktop-resource",
                            "name": "Desktop resource",
                            "mimeType": "text/plain",
                        }
                    ]
                },
            )
        elif method == "prompts/list":
            response(
                request_id,
                {
                    "prompts": [
                        {
                            "name": "summarize",
                            "description": "Summarize the current input",
                        }
                    ]
                },
            )
        elif request_id is not None:
            error(request_id, -32601, "method not found")


if __name__ == "__main__":
    main()
