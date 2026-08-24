# ChatShell reference audit for Aworkit specification completion

Date: 2026-08-23  
Reference repositories:

- `chatshellapp/chatshell-desktop` at commit `63aab8d2537e9f0c02e06a0878032bf3c6974282`
- `chatshellapp/chatshell-agent` at commit `c4e1e170394648a5936899ee0fb29854027979a7`

Both references were checked out read-only from their canonical GitHub repositories. No
ChatShell source, packages, assets, styles, state, or build configuration are copied into
Aworkit. This audit identifies behavior to reimplement behind Aworkit's own contracts.

## Why the reference was missed

The original implementation plan recorded ChatShell as reference-only, but interpreted
that boundary as a reason not to inspect the donor implementation at all. The Phase-0
recovery then replaced the required capability configuration with one OpenAI-compatible
endpoint and deliberately disabled the remaining Settings sections. Those are separate
errors:

1. Reference-only forbids accidental architectural or source coupling; it does not forbid
   reading the reference and learning from working interaction patterns.
2. A temporary one-provider recovery slice cannot be represented as completion of the
   specification's provider, model-tier, credential, tool, extension, MCP, external-agent,
   data, project, and appearance settings.
3. Horizontal contract tests did not require the native desktop to connect those settings
   to production effects.

## Provider and model setup

ChatShell has a genuine provider/model editor. The main reference paths are:

- `src/components/provider-settings-dialog/llm-provider-settings.tsx`
- `src/components/provider-settings-dialog/provider-form.tsx`
- `src/components/provider-settings-dialog/models-table.tsx`
- `src/components/provider-settings-dialog/fetch-models-dialog.tsx`
- `src/components/provider-settings-dialog/useProviderSave.ts`
- `src/components/provider-settings-dialog/constants.ts`
- `src-tauri/src/commands/providers.rs`
- `src-tauri/src/commands/models.rs`
- `src-tauri/src/commands/model_fetch.rs`
- `src-tauri/src/db/providers.rs`

Useful behavior to reimplement:

- a provider catalog plus a clear custom-provider path;
- editable compatibility/API style, base URL, name, description, and enabled state;
- a model table with create, edit, delete, search, discovery, retry, and manual fallback;
- a connection test with visible progress, latency, selected-model identity, and error;
- separate provider and model identities rather than one global endpoint/model pair.

Behavior Aworkit must not copy:

- ChatShell returns decrypted API keys to renderer state and permits revealing them;
- its provider/model save is a sequence of independent mutations and can partially commit;
- connection success is not bound to the exact subsequently saved draft;
- validation and persistent health facts are incomplete;
- an unavailable keychain can degrade to ephemeral in-memory key handling;
- provider objects containing secrets can reach frontend logging.

Aworkit keeps opaque credential references, a dedicated write-only secret command,
version-checked configuration commits, exact draft fingerprints for tests, explicit secure
store health, and no plaintext secret projection.

## Built-in tools

ChatShell exposes nine built-in tool switches in `src/components/settings-dialog.tsx` and
seeds them in `src-tauri/src/db/tools.rs`. The clear inventory, enable-all/disable-all,
per-tool status, and dependency explanation are useful UI references.

It is not an authority reference. ChatShell's path policy explicitly does not enforce the
working directory as a read boundary. Aworkit therefore cannot present that behavior as
Project Files. Aworkit tool settings must expose the real authority mode, enforced project
roots, platform availability, side-effect class, approval behavior, limits, and test or
health state. The runtime must use the existing root-confined Rust file adapters and
bounded process adapters rather than a working-directory convention.

## MCP servers

The strongest reusable interaction pattern is ChatShell's per-server lifecycle card in
`src/components/settings-dialog.tsx` and its configuration dialog in
`src/components/mcp-server-config-modal.tsx`. Supporting files include
`src/stores/mcpStore.ts`, `src/types/tool.ts`, `src-tauri/src/commands/mcp.rs`, and
`src-tauri/src/mcp/{manager,oauth}.rs`.

Useful behavior to reimplement:

- HTTP and STDIO transports;
- name, endpoint or command, arguments, working directory, environment, headers, and
  description;
- inert JSON-to-form import;
- idle, connecting, connected, authorization-required, and error states;
- explicit connect/reconnect and OAuth actions;
- expandable discovered tools/resources/prompts with visible server errors;
- safe deletion confirmation and accessible action labels.

Behavior Aworkit must not copy:

- environment and header values stored as arbitrary plaintext configuration;
- raw configuration written to debug logs;
- save dialogs that close before connection outcome is known;
- mutations whose errors are only logged;
- weak draft validation and unconfirmed deletion;
- implicit connection or activation resulting from import.

Aworkit configuration stores only non-secret values and credential references. Import is
inert, save and enable are distinct, discovery never grants workflow authority, and all
server calls remain generation- and schema-bound through the existing MCP session
contracts.

## Other Settings areas

ChatShell is not a reference implementation for the rest of Aworkit's Settings scope:

- credentials are not a first-class, write-only reference manager;
- model tiers do not exist;
- Projects and enforced workspace bindings do not exist;
- Data and portable-session configuration do not exist;
- Appearance is commented out in the Settings navigation;
- trusted extensions/plugins in Aworkit's sense do not exist;
- external agents with progress, continuation, cancellation, approvals, and native session
  identity do not exist.

ChatShell's Assistant editor is an ordinary profile editor, not an external-agent adapter.
Its helpful pattern is to keep unavailable tools and skills visible with a reason and a
route to Settings. Aworkit must implement its own agent profile, model-tier, context,
limits, result-contract, and explicit external-agent lifecycle surfaces.

## Companion agent engine

The companion `chatshell-agent` repository provides a provider-neutral streaming loop and
host tool-call events. It does not own Settings, persistence, credential brokerage, MCP,
projects, portable sessions, or external-agent lifecycle. It is useful for comparing
provider request normalization and streaming state, but it cannot replace Aworkit's
workflow worker, authority, semantic history, or recovery model.

## Test audit

ChatShell has store and backend unit tests but no rendered Settings/provider/MCP component
test suite and no native Settings end-to-end gate. Aworkit's completion gate must therefore
be stricter than the reference:

1. Every visible Settings control reaches the native trusted-core command path.
2. Configuration and non-secret health state survive a new process.
3. Secrets never appear in canonical JSON, projections, logs, receipts, or screenshots.
4. Test/discovery results are tied to an exact draft fingerprint and cannot silently bind
   a changed draft.
5. Failure and version-conflict paths preserve drafts and show actionable errors.
6. Enabling a tool, MCP server, extension, or external agent changes the capability
   inventory but never expands an already frozen Chat.
7. One native Simple Chat uses configured provider/model-tier resolution and can execute
   explicitly enabled tools or MCP calls through the authority boundary.

## Correct reuse boundary

ChatShell supplies interaction evidence for provider/model CRUD, model discovery,
connection tests, tool inventories, MCP configuration, and lifecycle presentation.
Aworkit independently implements those behaviors in its own React components, typed
commands, canonical JSON documents, trusted-core services, capability host, workflow
worker, semantic history, and native acceptance tests. Aworkit remains a workflow product;
it is not rebuilt as ChatShell.
