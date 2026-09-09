"""Deterministic, project-independent MCP evidence for native context checks."""
import json
import sys

TOOLS = [
    {"name": "list_tasks", "description": "List fixture tasks filtered by state.",
     "inputSchema": {"type": "object", "properties": {"projectId": {"type": "string"},
         "states": {"type": "array", "items": {"enum": ["open", "finished"]}}}, "required": ["projectId"]}},
    {"name": "large_result", "description": "Read complete fixture task evidence.",
     "inputSchema": {"type": "object", "properties": {}}},
]
IMAGE = {"type": "image", "mimeType": "image/png", "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIHWP4z8DwHwAFgAI/ScLbtAAAAABJRU5ErkJggg=="}
ANNOTATED = {"type": "text", "text": "Distinct annotated explanation.", "annotations": {"audience": ["user"]}}

for line in sys.stdin:
    if not line.strip():
        continue
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request.get("method")
    if method == "initialize":
        result = {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}},
                  "serverInfo": {"name": "context-efficiency-fixture", "version": "1.0"}}
    elif method == "tools/list":
        result = {"tools": TOOLS}
    elif method == "tools/call":
        params = request["params"]
        if params["name"] == "list_tasks":
            if params["arguments"].get("states") != ["open"]:
                raise RuntimeError("The complete empty result must not trigger an unfiltered query")
            data = {"projectName": "Aworkit", "tasks": [], "total": 0, "hasMore": False}
            result = {"isError": False, "structuredContent": data,
                      "content": [{"type": "text", "text": json.dumps(data)}]}
        else:
            tasks = [{"id": i, "state": "finished", "description": ("Task evidence with Unicode é🦀 and quotes \". " * 120)} for i in range(40)]
            tasks[-1]["description"] += " OMITTED_RECEIPT_739"
            result = {"isError": False, "structuredContent": {"tasks": tasks, "total": 40, "tail": "FINAL_TASK_MARKER"},
                      "content": [{"type": "text", "text": "Long explanation. " * 1500}, IMAGE, ANNOTATED]}
    else:
        print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "error": {"code": -32601, "message": "method not found"}}), flush=True)
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}, ensure_ascii=False), flush=True)
