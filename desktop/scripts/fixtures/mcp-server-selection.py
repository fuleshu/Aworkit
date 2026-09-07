"""Two-function MCP server for native server-selection regression checks."""
import json
import sys


for line in sys.stdin:
    if not line.strip():
        continue
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request.get("method")
    if method == "initialize":
        result = {
            "protocolVersion": "2025-11-25",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "server-selection-fixture", "version": "1.0"},
        }
    elif method == "tools/list":
        result = {"tools": [
            {"name": name, "description": "Return a message" if name == "echo" else "Describe this fixture",
             "inputSchema": {"type": "object", "properties": {"message": {"type": "string"}}}}
            for name in ["echo", "describe"]
        ]}
    elif method == "tools/call":
        params = request.get("params", {})
        text = params.get("arguments", {}).get("message", "two-function fixture")
        result = {"content": [{"type": "text", "text": text}], "isError": False}
    else:
        print(json.dumps({"jsonrpc": "2.0", "id": request["id"],
                          "error": {"code": -32601, "message": "method not found"}}), flush=True)
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
