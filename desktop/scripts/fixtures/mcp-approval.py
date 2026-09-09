"""Local MCP annotation matrix; each invocation records an observable fixture effect."""
import json
from pathlib import Path
import sys

HINTS = {
    "read_safe": {"readOnlyHint": True, "destructiveHint": False},
    "write_claim": {"readOnlyHint": False, "destructiveHint": False},
    "destructive": {"readOnlyHint": True, "destructiveHint": True},
    "only_read": {"readOnlyHint": True},
    "only_non_destructive": {"destructiveHint": False},
    "unannotated": None,
}

for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request.get("method")
    if method == "initialize":
        result = {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}},
                  "serverInfo": {"name": "approval-fixture", "version": "1.0"}}
    elif method == "tools/list":
        result = {"tools": [dict({"name": name, "description": "Approval fixture " + name,
                    "inputSchema": {"type": "object", "properties": {"label": {"type": "string"}}, "required": ["label"]}},
                    **({"annotations": hints} if hints is not None else {})) for name, hints in HINTS.items()]}
    elif method == "tools/call":
        params = request["params"]
        receipt = {"tool": params["name"], "label": params["arguments"]["label"]}
        with Path(sys.argv[1]).open("a", encoding="utf-8") as audit:
            audit.write(json.dumps(receipt) + "\n")
        result = {"content": [{"type": "text", "text": json.dumps(receipt)}], "isError": False}
    else:
        print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "error": {"code": -32601, "message": "method not found"}}), flush=True)
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
