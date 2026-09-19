#!/usr/bin/env python3
import json
import os
import subprocess
import sys


def console_state():
    """Observe real Windows console allocation, including an ordinary child."""
    if sys.platform != "win32":
        return {"rootConsole": 0, "childConsole": 0}
    code = "import ctypes; k=ctypes.windll.kernel32; k.GetConsoleWindow.restype=ctypes.c_void_p; print(k.GetConsoleWindow() or 0)"
    import ctypes
    kernel = ctypes.windll.kernel32
    kernel.GetConsoleWindow.restype = ctypes.c_void_p
    return {
        "rootConsole": kernel.GetConsoleWindow() or 0,
        "childConsole": int(subprocess.check_output([sys.executable, "-c", code], text=True, timeout=5)),
    }


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
    audit = os.environ.get("AWORKIT_CONSOLE_AUDIT")
    if "--console-audit" in sys.argv:
        audit = sys.argv[sys.argv.index("--console-audit") + 1]
    if audit:
        with open(audit, "a", encoding="utf-8") as output:
            output.write(json.dumps({"pid": os.getpid(), **console_state()}) + "\n")
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
                            "annotations": {"readOnlyHint": True, "destructiveHint": False},
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
            if message_text == "__console_probe__":
                message_text = json.dumps(console_state())
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
    if "--console-probe" in sys.argv:
        print(json.dumps(console_state()))
    else:
        main()
