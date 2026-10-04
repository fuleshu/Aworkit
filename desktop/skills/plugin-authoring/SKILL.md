---
name: plugin-authoring
description: Build an Aworkit tool plugin: package layout, declaration, an MCP server (local or HTTP), configuration, credentials, schemas, optional skills, local testing and common pitfalls.
---

# Build an Aworkit tool plugin

Read this before creating or changing a tool plugin. The canonical architecture
is the Adashi document `aworkit.capability_host.tool_plugins`; this skill is its
short, actionable form.

## 1. Pick the right layer

- Pure procedure, no typed input, no side effect -> a **skill** (`SKILL.md`), not a plugin.
- A capability with a typed input, a result and a possible side effect -> a **tool** inside a plugin.
- One install/trust unit that bundles one or more MCP servers and optional skills -> a **plugin** folder.

A plugin adds authority only through the existing binding and approval path. It
cannot widen what a Chat may do.

## 2. Package layout

```
my-plugin/
  tool-plugin.json
  server.py            # or a shipped executable
  skills/my-skill/SKILL.md   # optional
  README.md
```

One plugin per folder. The folder is copied whole, so the server may read
relative files.

## 3. Write the declaration

```json
{
  "schemaVersion": 1,
  "id": "plugin.my-plugin",
  "name": "My plugin",
  "version": "1.0.0",
  "execution": { "transport": "stdio", "command": "python", "args": ["server.py"], "env": [] },
  "tools": [
    { "name": "my_tool", "description": "What it does.",
      "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"], "additionalProperties": false },
      "enabled": true,
      "annotations": { "readOnlyHint": false, "destructiveHint": false },
      "options": { "instructions": "Say when the model should use this tool." } }
  ]
}
```

- `id` is stable across versions; workflows bind to it.
- `version` changes when behaviour changes.
- A bare `command` (`python`, `node`) resolves from `PATH`. A command with a
  separator (or `./name`) resolves inside the package folder and must not escape
  it. An absolute command selects an installed program.
- `args`, `cwd` and a config file the server reads are how a plugin carries its
  own settings. Never put a secret in any of them.
- The optional seed `tools` catalog is a preview and a place for instructions.
  The server's live `tools/list` is authoritative.

## 4. Implement the MCP server (local)

A local server speaks MCP over standard input and output: one JSON-RPC 2.0
message per line, diagnostics on standard error. Implement `initialize`,
`tools/list` and `tools/call`; ignore notifications; answer unknown methods with
a JSON-RPC error so a modern client falls back to `initialize`.

```python
for line in sys.stdin:
    message = json.loads(line)
    if message.get("id") is None:
        continue                      # notification
    method = message["method"]
    if method == "initialize":
        reply(message["id"], {"protocolVersion": "2024-11-05",
                              "capabilities": {"tools": {}},
                              "serverInfo": {"name": "my-plugin", "version": "1.0.0"}})
    elif method == "tools/list":
        reply(message["id"], {"tools": TOOL_DEFINITIONS})
    elif method == "tools/call":
        # ... do the work ...
        reply(message["id"], {"content": [{"type": "text", "text": "..."}],
                              "structuredContent": result, "isError": False})
    else:
        error(message["id"], -32601, f"Method not found: {method}")
```

Flush every reply. Keep standard output pure JSON.

## 5. Implement the MCP server (HTTP)

For a service, replace `execution` with
`{ "transport": "http", "url": "https://example.com/mcp", "headers": [] }` and
implement streamable HTTP MCP. Add a credential binding to an `Authorization`
header in Settings rather than embedding a token.

## 6. Configuration and credentials

- Configuration is ordinary text: command arguments, a working directory, a URL,
  or a file the server reads. Ship a documented default.
- Credentials are named references only. `env` (stdio) and `headers` (HTTP) name
  a stored credential field; the value is materialised for the connection alone.
  Read it from the environment variable (`os.environ.get("MY_API_KEY")`) or the
  header.
- Never log a secret, never echo it in a tool result, and never write it into
  `tool-plugin.json`.

## 7. Schemas and results

- Give every tool a bounded, closed `inputSchema`
  (`"additionalProperties": false`) and required fields.
- Return `structuredContent` when you have structured data, plus a short
  human-readable `content` text. Keep both under the tool-result bound.
- Return `isError: true` with a clear message for a tool-level failure (an
  unreachable service, a bad argument). Do not raise and do not print a
  traceback to standard output.

## 8. Optional skills

Add `skills/<name>/SKILL.md` with YAML frontmatter:

```markdown
---
name: my-skill
description: When to use this knowledge.
---

# Steps
1. ...
```

Skills contribute knowledge; they do not add tools or authority.

## 9. Local testing

1. Run `python3 server.py` with a tiny script that writes an `initialize` line
   and reads the reply; check standard output is one JSON line.
2. Add a `--selftest` mode that prints one JSON status result, so the plugin can
   be checked without the desktop app.
3. Test the degraded path: point the plugin at a stopped service and confirm the
   tool returns `isError: true` with a fix in the message.
4. In Aworkit: **Settings -> Tool Plugins -> Install plugin...**, **Refresh**,
   **Add plugin**, then connect and enable it in place (its command, arguments,
   credentials and functions are all configured in the same section), bind the
   tool to an Agent and call it once end to end.

The reference implementation is `desktop/tool-plugins/comfyui-bridge/`, with a
dependency-free end-to-end check in its `test_bridge.py`.

## 10. Common pitfalls

- Writing anything but JSON to standard output (a print, a warning, a banner)
  corrupts the MCP stream. Use standard error.
- Inventing settings that the server ignores; make every documented argument
  real.
- Returning a huge result; respect the configured tool-output bound.
- Assuming the plugin stays enabled after a copy: a newly sourced plugin is
  listed and turned off until the user adds and enables it.
- Treating a side-effect hint as a sandbox. It is a hint for approval, not a
  security boundary.
- Hard-coding an absolute interpreter path that only exists on your machine.
  Prefer a bare `python`/`node` name, or document the absolute path.
- Assuming the plugin folder must survive for an old workflow to keep working:
  a missing plugin is reported, and the Run continues.
