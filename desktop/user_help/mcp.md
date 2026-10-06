# MCP servers

Aworkit speaks the [Model Context Protocol](https://modelcontextprotocol.io) as
its only external tool protocol. Any MCP server you configure is discovered,
its tools are listed, and you choose which ones an Agent node may call.

## Adding a server

Open **Settings → MCP servers**:

- **stdio (local)** — an executable command plus arguments, an optional working
  directory and optional environment bindings. The process speaks MCP JSON-RPC
  over standard input and output; diagnostics belong on standard error.
- **streamable HTTP (remote)** — a server URL plus optional header bindings.

Then connect the server to load its catalog, review the discovered tools, edit
per-tool instructions, choose which functions are enabled, and save. If the
connection fails or the catalog is empty, a setup error is shown and nothing is
disabled or rebound silently.

## Credentials

Secrets are **named references only**. `env` (stdio) and `headers` (HTTP) name a
stored credential field; the value is materialised for that connection alone and
is never written into configuration, logged, or echoed in a tool result.
Arguments that look like inline authentication material are refused.

## Approval and side effects

An MCP tool can declare `readOnlyHint` and `destructiveHint` annotations. A call
may run without review only when the hints say read-only *and* not destructive,
or when you explicitly set auto-approve for that tool. Hints are review
metadata, not a sandbox: your approval policy and the Chat's frozen authority
remain the boundary.

## What a Chat freezes

Each Chat keeps its own MCP transport and catalog snapshot. The first message
freezes the selected tool definitions, so later Settings edits apply to new
Chats only. Reopening a Chat reconnects its saved endpoint and credential
references and preserves its prompts and approval choices.

## Missing servers

A missing or unreachable server is an explicit, named failure — the tool call
reports the endpoint and the fix and the run continues with the capabilities
that are present. A workflow never silently drops a reference.

## Plugin-backed servers

A server that arrives inside a **tool plugin** is configured with its package on
the **Tool Plugins** tab instead, so one package is never split across two
sections. See [Tool plugins and skills](plugins.md).
