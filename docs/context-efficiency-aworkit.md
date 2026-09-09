# Aworkit: reduce unnecessary model context

Status: immediate fixes A1–A3 implemented and verified on 2026-09-08. Follow-up items A4–A6 remain pending.

## Problem

A simple operational question, `are there any open tasks for aworkit in adashi?`, accumulated substantially more context than the answer required. Aworkit repeats MCP descriptions in its system prompt, exposes a broad tool catalog, adds a separate planning step, and admits unnecessary follow-up retrieval. Its final output cap can then cut structured results in the middle of a value.

The goal is to reduce context before provider admission while preserving configured instructions, tool authority, original evidence, and access to omitted information.

## Evidence

The reviewed Chat started on 2026-09-08 at 18:13:14 UTC. Measurements below are characters of text or compact JSON, not provider token counts or cumulative billing.

| Observation | Evidence |
| --- | --- |
| Broad tool catalog | 47 tool definitions, including all 33 Adashi tools; approximately 36,688 characters of compact JSON. |
| Repeated descriptions | All 33 Adashi descriptions also appear verbatim in the system prompt: 5,634 repeated description characters, excluding identifier headings. |
| Large system prompt | 14,482 characters; 13,281 belong to the per-tool instruction section. |
| Unnecessary retrieval | The successful `states: ["open"]` query returned `tasks: []`. The agent subsequently requested all tasks and then confirmed tasks. |
| Incomplete result | The all-task result was 214,666 bytes. Aworkit capped it at 65,536 bytes, cutting a task description and returning incomplete JSON as text with a truncation notice. |
| Lifecycle mismatch | Workspace instructions required four hooks. The export contains only `run.start` and `run.end`; `run.start` was requested alongside the task query. |
| Retrieval unavailable | The frozen tool definitions contain no `aworkit_context`. This does not establish whether compression was disabled or which policy was active. |

Aworkit already removes an MCP text block when its parsed JSON exactly equals `structuredContent`. The remaining duplicated Adashi memory is inside that structured object and belongs to the separate Adashi fix.

## TODO: immediate fixes

- [x] **A1 — Remove the automatic copy of MCP descriptions into system instructions.** Keep ordinary descriptions in tool definitions. Preserve distinct, explicitly configured tool instructions. Avoid emitting empty per-tool headings; retain a compact identity mapping only where runtime behavior requires it.
- [x] **A2 — Treat complete filtered results as sufficient evidence.** Adjust applicable agent guidance so a successful, complete empty result answers an existence query. Broaden retrieval only for a concrete ambiguity, error, partial result, or user request. Do not introduce a hard limit on tool calls or special-case Adashi task names.
- [x] **A3 — Preserve structure when tool output exceeds its limit.** Use the existing compression and retrieval path where configured and authorized. Otherwise return a bounded, valid projection with explicit omissions and a narrower-query instruction. Do not leave a JSON object cut in the middle of a value. Retain the canonical original and never claim a preview is complete.

## TODO: follow-up design and integration

- [ ] **A4 — Provide an appropriate tool set and execution path for simple lookups.** Start with existing Agent tool selections and workflow composition. Support a direct tool-using Agent path without a mandatory separate Plan model call. If deferred schema loading is needed, restrict it to the already authorized frozen catalog; loading a schema must not grant new authority. Preserve workflows that intentionally include planning.
- [ ] **A5 — Make configured lifecycle hooks reliable and economical.** Decide the smallest generic integration needed to evaluate applicable hooks in order and apply their instructions before dependent work. Keep empty-hook bookkeeping outside model messages where possible. Preserve the configured four-hook contract unless it is explicitly changed; do not implement an Adashi-specific policy in the generic MCP adapter.
- [ ] **A6 — Expose actionable context costs.** Extend context inspection with contributions by source/tool, duplicate sizes, and truncation or retrieval status. Distinguish provider-bound context from diagnostic evidence, estimates from measured tokens, and per-request footprint from cumulative usage. Reuse the existing inspector and preserve credential exclusion.

## Acceptance checks

- [x] A captured provider request includes each ordinary MCP description once; custom instructions remain intact.
- [x] The original open-task question receives the empty filtered result and an accurate answer without an unfiltered task dump. Use this as a behavior scenario, not a claim that one fixture guarantees all model behavior.
- [x] Oversized structured results remain valid and explicitly partial; configured retrieval recovers omitted content exactly.
- [x] Error status and fitting distinct MCP text, annotations and media survive projection. Oversized opaque blocks are omitted whole with explicit partial-result metadata and preserved in canonical evidence. No historical tools are replayed and no grants or frozen definitions are silently changed.
- [ ] Hook ordering and instruction application are verified for a nonempty hook as well as an empty hook.
- [x] Verify the affected path through the native workflow editor and actual provider request, with before/after measurements using the same fixture and settings. Exercise restart where durable projections change.

## Implementation and verification

New Chats freeze only explicit MCP instructions; existing frozen instruction snapshots are preserved. Agent context omits empty native-tool headings and keeps a compact MCP alias/capability mapping where no custom instruction section supplies the identity. Generic query guidance is added to tool-using Agent context without changing tool-call limits or available authority.

The output cap now produces `aworkitOutput` metadata and valid partial JSON under `preview`. It shares the encoded-byte budget across sibling bodies, reserves space for fitting metadata/media, and marks shortened strings. Optional compression still runs first. If an eligible oversized result cannot be compressed to fit and Context retrieval is authorized, the existing archive commits its original before publishing a reference. This mandatory cap fallback is recorded as `output-limit-preview`, explicitly lossy even when optional compression is lossless. No reference or tool grant is invented when retrieval is unavailable. See `docs/context-compression.md` for the fallback contract.

Validation:

- 290 desktop Rust tests passed, including frozen MCP settings, exact compatibility deduplication, bounded Unicode/escaped JSON, fitting media/annotations, durable preview replay, and recovery of an omitted original tail.
- TypeScript checking, Vite asset build, and native executable compilation passed. The native executable was built separately while the regular debug executable was open.
- `desktop/scripts/native-context-efficiency.mjs` passed against the real WebView, editor Validate/Save/Run, production MCP transport, and a deterministic loopback provider. The empty query scenario used one MCP call. Both preview variants reduced the same 264,487-byte result to valid 4,096-byte JSON; fitting media and metadata remained intact. An unchanged preview and an exact omitted receipt were recovered after process-tree restart.
- Evidence: `desktop/src-tauri/target/native-context-efficiency-1788895290618/report.json`, with provider requests and screenshots in the same directory. The provider is scripted: this validates runtime delivery and the scenario, not a guarantee that every live model will follow the new query guidance.
- `git diff --check` passed. Lifecycle ordering belongs to A5 and was not changed or claimed verified here.

Reproduce with `cargo test --manifest-path desktop/src-tauri/Cargo.toml --lib -- --test-threads=4`, build `desktop/dist` and the native binary, then run `node scripts/native-context-efficiency.mjs` from `desktop`. Set `AWORKIT_QA_BINARY` if using a separate executable; `AWORKIT_QA_PYTHON` can select an absolute Python interpreter for the local MCP fixture. The regression owns an isolated profile, loopback ports, and its own process tree.

## Implementation starting points

- `desktop/src-tauri/src/runtime/service.rs`: MCP description copied into `options.instructions`.
- `desktop/src-tauri/src/runtime/graph_pass/context.rs`: per-tool system sections and upstream Plan context.
- `desktop/src-tauri/src/runtime/service/mcp_selection.rs`: server selection expands to enabled functions.
- `desktop/workflows/default-workflows.json`: Standard Agent planning and tool selection.
- `desktop/src-tauri/src/runtime/model_tool_loop.rs`: model-facing output cap.
- `desktop/src-tauri/src/runtime/mcp_tools/result.rs`: existing exact MCP compatibility deduplication.
- `desktop/src-tauri/src/runtime/compression/` and `docs/context-compression.md`: existing projection and retrieval machinery.

## Ownership boundary

Adashi owns duplicate fields inside its responses, task-list projections/pagination, and the amount and quality of generated memory/design context. Aworkit should not infer that arbitrary repeated fields from an MCP server are semantically interchangeable. Coordinate response-contract changes with Adashi and preserve supported existing clients and frozen Chats.
