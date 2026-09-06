# Tool plugins and prompt composition

Aworkit uses a short, editable workflow persona. Each selected tool contributes
its own versioned instructions. The Run freezes the persona, selected tool
definitions, instructions, settings, execution identity, and approval policy;
later Settings edits apply to new Chats.

## Registry

Bundled tool plugins live in `desktop/tool-plugins/<plugin>/tool-plugin.json`.
Their manifests describe identity, native executor, model-facing description,
instructions, model input schemas, configuration defaults, and labelled configuration fields.
Native executors continue to enforce their actual authority and argument
contracts; editing a manifest cannot invent an implementation or sandbox.

External tool plugins use MCP over stdio (an executable plus arguments) or
streamable HTTP (a server endpoint). Folder scans never execute code. The explicit
Discover and test action opens a temporary connection, including for a disabled
draft; enabling makes its tools available to workflows. MCP is the external tool
protocol; Aworkit does not introduce a second tool wire protocol.

The registry exposes the same tool identity/description/instructions contract
for native and MCP tools. Workflow tool selection lists both. Unavailable
references remain visible and fail explicitly instead of being dropped.

## Settings

Tools presents ordinary labelled controls generated from manifest settings,
including instructions, execution information, limits, and approval policy.
MCP retains its connection tab, with executable/arguments or server endpoint,
discovered tools, editable tool instructions, and per-tool enablement. A
discovery refresh preserves user overrides while updating server descriptions
and schemas. Secrets continue through the existing credential-reference path.

Each Chat retains its own MCP transport and catalog snapshot. New Chats pick up
saved transport changes immediately. Reopening a Chat reconnects its saved
endpoint and credential references, checks the manifest binding and tool schemas,
and preserves its prompts and approval choices. Pending tool approval can also
resume after restart. A changed schema requires a New Chat; old Chats created
before connection snapshots were introduced cannot reconstruct a missing MCP
session from unrelated current Settings.

## Prompt composition

Only tools selected and available to the node contribute instructions, in
deterministic order. Tool guidance describes when and how to use that tool,
without requiring unrelated actions. Search discovers sources; retrieval reads
a supplied URL. Partial content is assessed before deciding on further work.
Subagents receive guidance for their own permitted tool set.

## Compatibility and validation

Known untouched legacy default personas migrate to the short persona; custom
workflow instructions remain unchanged. Old frozen Runs retain their original
serialized contracts. Verify manifest validation, prompt selection and freeze,
MCP discovery-to-selection-to-execution, typed Settings persistence, executable
resolution, approval behavior, and native desktop rendering.

## Writing an external tool plugin

Settings → Tools shows the profile's exact plugin folder. Put each package in
its own subfolder with this version-1 `tool-plugin.json`:

```json
{
  "schemaVersion": 1,
  "id": "plugin.example",
  "name": "Example tools",
  "version": "1.0.0",
  "execution": {
    "transport": "stdio",
    "command": "example-server.exe",
    "args": [],
    "env": []
  }
}
```

The command above resolves inside the package folder. An absolute command can
select an installed interpreter or executable. Arguments are passed individually,
without a shell. `cwd` is optional and defaults to the package folder; relative
command/cwd paths cannot traverse outside it. Use absolute paths in arguments
when referring outside the working directory. The process must speak MCP on
standard input/output; diagnostics belong on standard error.

For a service, replace `execution` with:

```json
{ "transport": "http", "url": "https://example.com/mcp", "headers": [] }
```

`env` and `headers` accept named credential references from Settings, never
embedded secrets. The MCP tab edits transport, command, arguments, directory,
credential bindings, tool enablement, instructions, and approval overrides.
Local subprocesses run with the user's OS permissions. No process sandbox is
provided by this transport. Native project-file tools retain their confined
project boundary, and their fixed authority fields cannot be weakened.

An optional `tools` array seeds tool guidance before discovery:

```json
[
  {
    "name": "echo",
    "description": "Return the supplied message.",
    "inputSchema": { "type": "object" },
    "enabled": true,
    "options": { "instructions": "Use echo when the task asks to repeat text." }
  }
]
```

The server's live schemas remain authoritative. Refreshing discovery replaces
the catalog while preserving matching tools' user instructions and approval
choices. New tools become selectable only through an enabled saved server;
the workflow must explicitly select them.

Adding a package records its manifest path, version and SHA-256 in a disabled
Settings draft. A changed manifest blocks new probes and runs until **Refresh
plugins → Load updated plugin**, followed by review, discovery, enablement and
Save. Loading an update resets the transport to the package declaration and
keeps matching tools' overrides. The pin covers the manifest, not every external
dependency or remote service implementation. Existing MCP runtime binding and
schema checks still apply. General extension node/provider contributions remain
on their separate protocol and lifecycle.

Native plugins are bundled at build time; changing their implementation or
shipped manifest requires rebuilding the application. User instruction, limit,
executable and approval edits need no rebuild. Unknown custom workflow personas
remain unchanged; only exact previous shipped personas migrate.

Regression commands: the desktop Rust library tests, capability-host
`milestone_05` tests, frontend tool/Settings/workflow tests, and
`node desktop/scripts/native-tool-plugins.mjs` (run from `desktop`, omitting the
`desktop/` prefix). The native script uses a temporary profile and local provider
and MCP fixtures, verifies provider-bound instructions and an actual tool result,
then restarts to check persistence. It requires the built Windows executable and
Python, configurable through `AWORKIT_QA_PYTHON`.
