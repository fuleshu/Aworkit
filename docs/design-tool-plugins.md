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
Enable MCP server checkbox (or Connect and enable button) opens a temporary
connection and loads functions before enabling the draft. Failed connections and
empty catalogs show a setup error. Save configuration publishes the result to
workflows. Changing the transport disables the draft until it is connected again.
MCP is the external tool
protocol; Aworkit does not introduce a second tool wire protocol.

The registry exposes the same tool identity/description/instructions contract
for native and MCP tools. Agent nodes show one checkbox per MCP server. A saved
`mcp:<server>` selection resolves to all enabled functions from the saved catalog
at first input; the frozen workflow contains exact `mcp://<server>/<tool>` bindings.
Later catalog edits affect only new Chats. Individual function bindings in older
workflows remain unchanged until the user selects the whole server. Tool nodes
still select one function because they execute one invocation. Unavailable
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

Project Chats pass their frozen project name, absolute directory and optional
Git branch to Plan and Agent model requests. These are context facts, not tool
arguments or permission grants. Aworkit's internal project id is not passed as
an MCP project id; external servers own their own identifiers. Reopening or
continuing a Chat uses its original project even after Settings edits.

Plan-contract nodes receive the actual tool inventories of the next reachable
Agents, grouped by node. Traversal stops at Agent, wait and completion boundaries.
Plans cannot add tools or require discovery of an inventory already supplied.
Agent guidance maps every callable alias to its exact capability identity,
including the full original MCP operation name. Initial execution and approval
recovery use the same context composition.

## MCP results

Structured MCP results retain their complete data. When a plain text block is an
exact JSON copy of `structuredContent`, the model continuation keeps only the
structured copy. Distinct or annotated text, media, metadata and `isError` are
preserved. Server-reported errors remain failed tool calls.

The 512 KiB tool-result bound applies after duplicate removal. A provider's
configured tool-output limit still clips larger model-facing results with an
explicit truncation notice. Completed exchanges have a separate 512 KiB durable
allowance; they are not charged again against the Agent's base input allowance.
Oversized nonduplicate results still fail explicitly.

`desktop/scripts/native-adashi-chat.mjs` tests the real Adashi stdio server in an
isolated native profile with a local deterministic provider. It checks project
context in Plan and Agent requests, the frozen tool inventory and aliases, and
delivery of the real `adashi_get_memory` structured result.

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
provided by this transport. Native file tools resolve an exact target before
execution. Paths inside the workspace need no approval; external paths use the
approval policy. Fixed authority fields cannot be weakened.

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

MCP provider aliases fit the portable 64-byte function-name limit. A digest of
the exact server and tool identifiers disambiguates folded punctuation and long
shared prefixes; MCP dispatch still uses the original capability and tool name.
Existing valid frozen aliases remain exact. Invalid aliases from older Chats are
repaired only in the wire projection, without rewriting history or frozen settings.

`node scripts/native-adashi-chat.mjs` (from `desktop`) verifies the live Adashi
catalog and one read-only rule lookup through the Standard Agent, using a local
deterministic provider and an isolated Chat profile. Set `AWORKIT_QA_ADASHI_EXE`
to the configured stdio executable. `node scripts/native-workflow-canvas.mjs`
checks initial painting, outline selection, edits, workflow switching and exact
Settings return in the actual WebView. Both accept `AWORKIT_QA_EXE`; when testing
a development frontend, set `AWORKIT_QA_PAGE_URL` to its origin.
