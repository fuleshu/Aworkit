#!/usr/bin/env python3
"""Aworkit reference tool plugin: a ComfyUI bridge.

This is a real MCP server over standard input/output. It speaks the
newline-delimited JSON-RPC framing of the Model Context Protocol and exposes
ComfyUI workflows as typed tools. Standard error carries diagnostics only; every
byte on standard output is one JSON-RPC message.

Configure it entirely from the plugin declaration and Settings:

  --endpoint       ComfyUI base URL (default http://127.0.0.1:8188)
  --workflows      JSON file or inline JSON mapping workflow id -> API workflow
  --api-key-env    Name of the environment variable holding a ComfyUI API key
  --timeout        Seconds to wait for one queued workflow (default 120)

The plugin never embeds a secret. The API key is read from the named variable,
which Settings binds to a stored credential reference.

Degraded behavior is explicit: when ComfyUI is unreachable, a tool call returns
an MCP error result whose text names the endpoint and the fix. The Run is not
ended; the model is told the plugin is unavailable and decides what to do next.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

PROTOCOL_VERSION = "2024-11-05"
SERVER_NAME = "comfyui-bridge"
SERVER_VERSION = "1.0.0"
CLIENT_ID = "aworkit-comfyui-bridge"
POLL_SECONDS = 0.75


def tool_definitions() -> list[dict]:
    return [
        {
            "name": "comfyui_status",
            "description": (
                "Report whether the configured ComfyUI server is reachable and "
                "list the workflow ids this plugin exposes."
            ),
            "inputSchema": {
                "type": "object",
                "additionalProperties": False,
                "properties": {},
            },
            "annotations": {"readOnlyHint": True, "destructiveHint": False},
        },
        {
            "name": "comfyui_list_workflows",
            "description": "List the ComfyUI workflow ids configured for this plugin.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": False,
                "properties": {},
            },
            "annotations": {"readOnlyHint": True, "destructiveHint": False},
        },
        {
            "name": "comfyui_run_workflow",
            "description": (
                "Queue one configured ComfyUI workflow, wait for it to finish and "
                "return the produced image references. Override node inputs with "
                "'<node id>.<input name>' keys."
            ),
            "inputSchema": {
                "type": "object",
                "additionalProperties": False,
                "properties": {
                    "workflow": {
                        "type": "string",
                        "description": "A workflow id returned by comfyui_list_workflows.",
                    },
                    "inputs": {
                        "type": "object",
                        "description": (
                            "Optional node input overrides keyed '<node id>.<input name>', "
                            "for example {\"6.text\": \"a quiet harbour\"}."
                        ),
                        "additionalProperties": True,
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "minimum": 5,
                        "maximum": 1800,
                    },
                },
                "required": ["workflow"],
            },
            "annotations": {"readOnlyHint": False, "destructiveHint": False},
        },
    ]


class ComfyUiError(Exception):
    """A failure that should be reported to the model, never a traceback."""


class Bridge:
    def __init__(self, endpoint: str, workflows: dict, api_key: str | None, timeout: float):
        self.endpoint = endpoint.rstrip("/")
        self.workflows = workflows
        self.api_key = api_key
        self.timeout = timeout

    # ---- HTTP -----------------------------------------------------------

    def _request(self, method: str, path: str, body: dict | None = None) -> dict:
        url = f"{self.endpoint}{path}"
        data = json.dumps(body).encode("utf-8") if body is not None else None
        request = urllib.request.Request(url, data=data, method=method)
        request.add_header("Accept", "application/json")
        if data is not None:
            request.add_header("Content-Type", "application/json")
        if self.api_key:
            request.add_header("Authorization", f"Bearer {self.api_key}")
        try:
            with urllib.request.urlopen(request, timeout=10) as response:
                payload = response.read()
        except urllib.error.HTTPError as error:
            detail = error.read().decode("utf-8", "replace")[:500]
            raise ComfyUiError(
                f"ComfyUI answered HTTP {error.code} at {url}. {detail}"
            ) from error
        except (urllib.error.URLError, OSError, TimeoutError) as error:
            raise ComfyUiError(
                f"ComfyUI is not reachable at {self.endpoint}. Start ComfyUI, or set "
                f"the endpoint in this plugin's settings. ({error})"
            ) from error
        try:
            return json.loads(payload.decode("utf-8")) if payload else {}
        except json.JSONDecodeError as error:
            raise ComfyUiError(f"ComfyUI returned a non-JSON response at {url}.") from error

    # ---- Tools ----------------------------------------------------------

    def status(self) -> dict:
        stats = self._request("GET", "/system_stats")
        return {
            "endpoint": self.endpoint,
            "reachable": True,
            "workflows": sorted(self.workflows),
            "system": stats.get("system", {}),
        }

    def list_workflows(self) -> dict:
        return {"workflows": sorted(self.workflows)}

    def _graph(self, workflow_id: str) -> dict:
        if workflow_id not in self.workflows:
            known = ", ".join(sorted(self.workflows)) or "none"
            raise ComfyUiError(
                f"Unknown workflow '{workflow_id}'. Configured workflows: {known}."
            )
        graph = self.workflows[workflow_id]
        if not isinstance(graph, dict):
            raise ComfyUiError(f"Workflow '{workflow_id}' is not a ComfyUI API graph.")
        return json.loads(json.dumps(graph))

    def run_workflow(self, arguments: dict) -> dict:
        workflow_id = arguments.get("workflow")
        if not isinstance(workflow_id, str) or not workflow_id:
            raise ComfyUiError("'workflow' is required and must be a configured workflow id.")
        graph = self._graph(workflow_id)
        overrides = arguments.get("inputs") or {}
        if not isinstance(overrides, dict):
            raise ComfyUiError("'inputs' must be an object of '<node id>.<input name>' overrides.")
        for key, value in overrides.items():
            if not isinstance(key, str) or "." not in key:
                raise ComfyUiError(
                    f"Input override '{key}' must be '<node id>.<input name>', for example '6.text'."
                )
            node_id, input_name = key.split(".", 1)
            node = graph.get(node_id)
            if not isinstance(node, dict):
                raise ComfyUiError(f"Workflow has no node '{node_id}' to override.")
            node.setdefault("inputs", {})[input_name] = value
        timeout = arguments.get("timeout_seconds", self.timeout)
        if not isinstance(timeout, (int, float)) or timeout < 5:
            timeout = self.timeout

        queued = self._request(
            "POST", "/prompt", {"prompt": graph, "client_id": CLIENT_ID}
        )
        prompt_id = queued.get("prompt_id")
        if not isinstance(prompt_id, str) or not prompt_id:
            raise ComfyUiError("ComfyUI did not return a prompt id when the workflow was queued.")

        deadline = time.monotonic() + float(timeout)
        while time.monotonic() < deadline:
            history = self._request("GET", f"/history/{urllib.parse.quote(prompt_id)}")
            entry = history.get(prompt_id)
            if isinstance(entry, dict) and entry.get("outputs") is not None:
                return self._result(workflow_id, prompt_id, entry)
            time.sleep(POLL_SECONDS)
        raise ComfyUiError(
            f"ComfyUI did not finish workflow '{workflow_id}' within {int(timeout)} seconds "
            f"(prompt {prompt_id}). It may still be queued."
        )

    def _result(self, workflow_id: str, prompt_id: str, entry: dict) -> dict:
        images: list[dict] = []
        for node_id, output in (entry.get("outputs") or {}).items():
            if not isinstance(output, dict):
                continue
            for image in output.get("images") or []:
                if not isinstance(image, dict) or not image.get("filename"):
                    continue
                query = urllib.parse.urlencode(
                    {
                        "filename": image.get("filename", ""),
                        "subfolder": image.get("subfolder", ""),
                        "type": image.get("type", "output"),
                    }
                )
                images.append(
                    {
                        "node_id": node_id,
                        "filename": image.get("filename"),
                        "subfolder": image.get("subfolder", ""),
                        "type": image.get("type", "output"),
                        "url": f"{self.endpoint}/view?{query}",
                    }
                )
        if not images:
            raise ComfyUiError(
                f"Workflow '{workflow_id}' finished but produced no image output. "
                "Check that the configured graph ends in a SaveImage node."
            )
        return {
            "workflow": workflow_id,
            "prompt_id": prompt_id,
            "images": images,
            "status": "completed",
        }

    def call(self, name: str, arguments: dict) -> dict:
        if name == "comfyui_status":
            return self.status()
        if name == "comfyui_list_workflows":
            return self.list_workflows()
        if name == "comfyui_run_workflow":
            return self.run_workflow(arguments)
        raise ComfyUiError(f"Unknown tool '{name}'.")


# ---- Configuration ------------------------------------------------------


def load_workflows(spec: str) -> dict:
    """Accepts an inline JSON object or a path to a JSON object of graphs."""
    text: str
    if spec.strip().startswith("{"):
        text = spec
        base = os.getcwd()
    else:
        path = spec if os.path.isabs(spec) else os.path.join(os.getcwd(), spec)
        try:
            with open(path, "r", encoding="utf-8") as handle:
                text = handle.read()
        except OSError as error:
            raise ComfyUiError(f"Could not read the workflow file '{spec}': {error}") from error
        base = os.path.dirname(path)
    try:
        mapping = json.loads(text)
    except json.JSONDecodeError as error:
        raise ComfyUiError(f"The workflow configuration is not valid JSON: {error}") from error
    if not isinstance(mapping, dict):
        raise ComfyUiError("The workflow configuration must be an object of id -> workflow.")
    resolved: dict = {}
    for workflow_id, value in mapping.items():
        if isinstance(value, str):
            graph_path = value if os.path.isabs(value) else os.path.join(base, value)
            try:
                with open(graph_path, "r", encoding="utf-8") as handle:
                    resolved[workflow_id] = json.load(handle)
            except (OSError, json.JSONDecodeError) as error:
                raise ComfyUiError(
                    f"Could not read workflow '{workflow_id}' from '{value}': {error}"
                ) from error
        elif isinstance(value, dict):
            resolved[workflow_id] = value
        else:
            raise ComfyUiError(f"Workflow '{workflow_id}' must be a file path or a JSON graph.")
    return resolved


# ---- MCP stdio loop -----------------------------------------------------


def write_message(message: dict) -> None:
    sys.stdout.write(json.dumps(message, separators=(",", ":"), ensure_ascii=False) + "\n")
    sys.stdout.flush()


def error_response(message_id, code: int, text: str) -> dict:
    return {"jsonrpc": "2.0", "id": message_id, "error": {"code": code, "message": text}}


def result_response(message_id, result: dict) -> dict:
    return {"jsonrpc": "2.0", "id": message_id, "result": result}


def tool_result(value: dict | None, text: str, is_error: bool = False) -> dict:
    result: dict = {
        "content": [{"type": "text", "text": text}],
        "isError": is_error,
    }
    if value is not None:
        result["structuredContent"] = value
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description="Aworkit ComfyUI bridge (MCP over stdio)")
    parser.add_argument("--endpoint", default=os.environ.get("COMFYUI_ENDPOINT", "http://127.0.0.1:8188"))
    parser.add_argument("--workflows", default=os.environ.get("COMFYUI_WORKFLOWS", "workflows.json"))
    parser.add_argument("--api-key-env", default="COMFYUI_API_KEY")
    parser.add_argument("--timeout", type=float, default=120.0)
    parser.add_argument(
        "--selftest",
        action="store_true",
        help="Print one JSON status result and exit instead of serving MCP.",
    )
    parser.add_argument(
        "--run",
        default=None,
        help="With --selftest, also queue this configured workflow id.",
    )
    args = parser.parse_args()

    try:
        workflows = load_workflows(args.workflows)
    except ComfyUiError as error:
        # A configuration failure still speaks MCP so Settings can report it as
        # a connection result instead of a dead process.
        workflows = {}
        sys.stderr.write(f"comfyui-bridge configuration: {error}\n")

    bridge = Bridge(
        endpoint=args.endpoint,
        workflows=workflows,
        api_key=os.environ.get(args.api_key_env) or None,
        timeout=args.timeout,
    )

    if args.selftest:
        try:
            output = {
                "status": bridge.status(),
                "workflows": bridge.list_workflows()["workflows"],
            }
            if args.run:
                output["result"] = bridge.run_workflow({"workflow": args.run})
            json.dump(output, sys.stdout, indent=2, sort_keys=True)
            sys.stdout.write("\n")
            return 0
        except ComfyUiError as error:
            sys.stderr.write(f"comfyui-bridge selftest: {error}\n")
            return 1

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            sys.stderr.write("comfyui-bridge: ignored a malformed JSON-RPC line\n")
            continue
        method = message.get("method")
        message_id = message.get("id")
        if method is None:
            continue  # A response to a server-initiated request; none are sent.
        if message_id is None:
            continue  # A notification, for example notifications/initialized.
        if method == "initialize":
            write_message(
                result_response(
                    message_id,
                    {
                        "protocolVersion": PROTOCOL_VERSION,
                        "capabilities": {"tools": {"listChanged": False}},
                        "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
                    },
                )
            )
        elif method in ("ping", "logging/setLevel"):
            write_message(result_response(message_id, {}))
        elif method == "tools/list":
            write_message(result_response(message_id, {"tools": tool_definitions()}))
        elif method == "tools/call":
            params = message.get("params") or {}
            name = params.get("name")
            arguments = params.get("arguments") or {}
            try:
                value = bridge.call(name, arguments if isinstance(arguments, dict) else {})
                write_message(
                    result_response(
                        message_id,
                        tool_result(
                            value,
                            json.dumps(value, indent=2, ensure_ascii=False, sort_keys=True),
                        ),
                    )
                )
            except ComfyUiError as error:
                write_message(
                    result_response(message_id, tool_result(None, str(error), is_error=True))
                )
            except Exception as error:  # Never expose a traceback to the model.
                sys.stderr.write(f"comfyui-bridge: tool '{name}' failed: {error}\n")
                write_message(
                    result_response(
                        message_id,
                        tool_result(None, f"The ComfyUI tool call failed: {error}", is_error=True),
                    )
                )
        else:
            # An unknown method is the documented signal that this server is
            # legacy, so a modern client retries with `initialize`.
            write_message(error_response(message_id, -32601, f"Method not found: {method}"))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except BrokenPipeError:
        raise SystemExit(0)
