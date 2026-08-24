#!/usr/bin/env python3
import json
import sys


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
    for line in sys.stdin:
        if not line.strip():
            continue
        message = json.loads(line)
        method = message.get("method")
        request_id = message.get("id")
        if method == "server/discover":
            return
        elif method == "initialize":
            response(
                request_id,
                {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "aworkit-test-mcp", "version": "1.0.0"},
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
                            "description": "Return one message",
                            "inputSchema": {
                                "type": "object",
                                "properties": {"message": {"type": "string"}},
                                "required": ["message"],
                            },
                        }
                    ]
                },
            )
        elif method == "tools/call":
            params = message.get("params", {})
            progress_token = params.get("_meta", {}).get("progressToken")
            if progress_token is not None:
                send(
                    {
                        "jsonrpc": "2.0",
                        "method": "notifications/progress",
                        "params": {
                            "progressToken": progress_token,
                            "progress": 1,
                            "total": 1,
                            "message": "echoed",
                        },
                    }
                )
            arguments = params.get("arguments", {})
            message_text = arguments.get("message", "")
            response(
                request_id,
                {
                    "content": [{"type": "text", "text": message_text}],
                    "structuredContent": {"echo": message_text},
                    "isError": False,
                },
            )
        elif method == "notifications/cancelled":
            continue
        elif request_id is not None:
            error(request_id, -32601, "method not found")


if __name__ == "__main__":
    main()
