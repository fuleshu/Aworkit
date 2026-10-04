---
name: aworkit-plugins
description: How Aworkit plugins and MCP servers work - the skill/tool/plugin layers, the plugin folder and tool-plugin.json, stdio and HTTP transports, configuration and credential bindings, enabling and trusting a plugin, side-effect hints, and install, update and removal behaviour.
---

# Aworkit plugins

Three layers, deliberately separate:

| Layer | What it is | Adds |
| --- | --- | --- |
| **Skill** | Markdown instructions (`SKILL.md`) | Knowledge only - see the `aworkit-skills` skill |
| **Tool** | A capability with a typed input, a result and possibly a side effect | An action the model can propose |
| **Plugin** | One installable folder that packages MCP servers, their tools and optional skills | The delivery unit for tools |

A plugin **cannot widen authority**. Selecting a tool in a workflow, enabling it
in Settings, and the approval policy are three separate decisions; a plugin only
supplies the capability.

## 1. The package

One plugin is one folder containing `tool-plugin.json` plus whatever the server
needs (a script, a binary, a README, optional `skills/<name>/SKILL.md`):

```json
{
  "schemaVersion": 1,
  "id": "plugin.ffmpeg",
  "name": "FFmpeg media tools",
  "version": "1.0.0",
  "execution": { "transport": "stdio", "command": "python", "args": ["ffmpeg_bridge.py"], "env": [] },
  "tools": [
    { "name": "ffmpeg_probe", "description": "Inspect a media file with ffprobe.",
      "inputSchema": { "type": "object", "properties": { "input": { "type": "string" } }, "required": ["input"], "additionalProperties": false },
      "enabled": true,
      "annotations": { "readOnlyHint": true, "destructiveHint": false },
      "options": { "instructions": "Probe a file before transforming it." } }
  ]
}
```

- `id` is stable across versions; saved workflows bind to it. `version` changes
  when behaviour changes.
- `tools[]` is a seed catalog and a place for per-tool `instructions`; the
  server's live `tools/list` is authoritative.
- A bare `command` (`python`, `node`) resolves from `PATH`; a command with a
  separator resolves inside the plugin folder and may not escape it; a relative
  `cwd` resolves inside the folder. Never put a secret in `command`, `args`,
  `cwd` or `env`.

## 2. Transports

- **stdio (local):** `{"transport":"stdio","command":...,"args":[...],"env":[...],"cwd":...}`.
  The server speaks MCP JSON-RPC over standard input/output, one message per
  line, diagnostics on standard error.
- **HTTP (server):** `{"transport":"http","url":"https://host/mcp","headers":[...]}`.
  Use a credential binding for the `Authorization` header instead of embedding
  a token.

## 3. Discovery, installation and trust

- The plugin folder is `tool-plugins` inside Aworkit's data directory
  (`runtime/tool-plugins` under the app data root). Aworkit scans its direct
  subfolders; two plugins with the same `id` are refused as duplicates.
- Installing copies the chosen folder into that directory - and nothing else.
  A copied plugin is **listed but disabled** until the user adds and enables it
  in Settings, so installation never executes or grants anything.
- Installation pins the manifest path, its content hash and its version. If the
  folder changes afterwards, Settings reports "This plugin changed. Refresh the
  plugins list, then add its new version in Settings." - an edit is not adopted
  silently.
- Enabling a tool is availability, not authority: filesystem grants, approval
  policy and frozen per-Chat bindings are unaffected.

## 4. Configuration and credentials

- Configuration is ordinary document state: command arguments, a working
  directory, a URL, or a file the server reads. It is edited in Settings and
  saved with a version check.
- Credentials are **named references only**. `env` (stdio) and `headers` (HTTP)
  name a stored credential field; the value is materialised for that connection
  alone, never written into `tool-plugin.json`, never logged and never echoed
  in a tool result.

## 5. Side-effect hints

A tool's `annotations.readOnlyHint` and `destructiveHint` are the server's own
statement about the call. Aworkit treats a call as approvable without review
only when the hints say `readOnlyHint: true` **and** `destructiveHint: false`,
or when the user has explicitly set auto-approve for that MCP tool. A hint is
review metadata, not a sandbox.

## 6. Updating and removing

- Update by installing the new version over the same folder: the id stays, the
  version and hash change, and Settings must accept the new version again.
- Removing deletes the installed folder. Workflows that referenced the plugin
  keep their configuration and remain inspectable and editable; a missing
  server or tool is reported as a warning and the Run continues with the
  capabilities that are present.

For building a plugin - implementing the server, schemas, local testing and the
common pitfalls - load the `plugin-authoring` skill, which covers that in
detail. This skill is about how the mechanism behaves.
