# Workspace Instructions plugin: implementation specification

Status: implemented, 2026-09-08. This records the user's decisions from the DeepSeek Harness comparison: selectable plugin tool per Agent node, instruction-loader behavior, lightweight persistence using existing history, and a mandatory, testable integration contract for future context compaction. See [operation and verification](workspace-instructions.md) for implementation boundaries and acceptance evidence. Automatic conversation compaction remains separate future work; the loader's restoration algorithm is implemented.

Reference: `C:/src/deepseek-harness` at `b150a551b8d465e31e418e1b2eaf5e79bbb7d28e`, particularly `packages/context/agent-instructions/{README.md,src,tests}` and `packages/bundle/base/cordis.patch.yml`. The feature target is that loader's observable behavior. The following explicitly documented Aworkit adaptations apply: `.aworkit` replaces `.dsh`; activation is selected per Agent node; existing workspace authority and frozen Chat policy remain authoritative. Aworkit does not currently have an automatic conversation compactor.

## 1. Product and plugin contract

- Register **Workspace Instructions**, ID `tool.workspace_instructions`, in the existing bundled native tool-plugin catalog. It is independently configurable from `tool.skill` and appears in the same Agent tool selector.
- Settings controls plugin availability and defaults. Each Agent node selects whether to activate it. An unselected plugin performs no instruction discovery or automatic injection for that node. Explicit user-provided context remains governed by existing context selection.
- Freeze selected plugin identity, configuration, resolved home paths, and workspace authority with the Chat. Settings edits affect new Chats; they do not alter an existing Chat's loader configuration. Existing Chats and saved workflows are not silently enabled or rebound during migration.
- Extend the manifest contract with an activation discriminator: ordinary entries default to `model_call`; this entry uses `automatic_context`. Automatic entries declare a supported native lifecycle executor and configuration, but no model-call function name or input schema. Validate these alternatives as a closed contract.
- Freeze both categories of selected bindings, but expose only callable entries in provider tool schemas. An automatic plugin must work when it is the Agent's only selected plugin and when the model supports text but not tool calling. It must not consume a model tool call or force a tool-capable provider.
- Selecting it activates lifecycle callbacks automatically. Do not depend on the model calling a function, reading the file voluntarily, or following a prompt asking it to load its own rules.
- The plugin is an Agent binding. A standalone Tool node referencing it should receive a specific validation error explaining that automatic context plugins belong on Agent nodes. Native admission, Settings, frontend validation, workflow templates, and frozen bindings must agree on the manifest contract.
- Installation/discovery remains inert. This extension does not add arbitrary native code loading or turn an MCP tool into an automatic context provider.

## 2. Complete behavior inventory

| ID | Required behavior | Implementation and acceptance meaning |
|---|---|---|
| WI-01 | Initial instruction chain | Before the first eligible model request, load global instructions, then the project-root-to-session-cwd chain in broad-to-specific order. Nearest configured project marker wins; `.git` may be a directory or worktree file; fallback is cwd. Respect the authorized workspace boundary. |
| WI-02 | Aworkit paths and portable files | Global file is `~/.aworkit/AGENTS.md`; configured homes expand `~`, `~/`, and Windows `~\\`. Project files remain ordinary Markdown, without mandatory YAML frontmatter. No `.dsh` or `DSH_HOME` discovery, fallback, or compatibility alias. A projectless Chat can load global instructions without treating its internal scratch directory as a user project. |
| WI-03 | Candidates and local overlays | In each project directory load every existing base candidate, by default `AGENTS.md`, `CLAUDE.md`, then every local overlay, by default `AGENTS.local.md`, `CLAUDE.local.md`. Candidate lists are ordered and configurable; an empty overlay list disables overlays. Global discovery uses only `AGENTS.md`, without overlays. |
| WI-04 | Deduplication | Suppress identical paths and same-directory candidates whose contents match after trimming surrounding whitespace. The earliest configured candidate wins, and its original content is rendered. Distinct siblings both load. A previously visible sibling that becomes a duplicate receives a removal transition. |
| WI-05 | Prompt and precedence fidelity | Use the reference baseline/addition/replacement/removal/superseding-baseline templates and complete user-role `<system-reminder>` framing. Keep directory scope explicit. More-specific guidance takes precedence; file instructions cannot override system, developer, or direct user instructions or grant authority. Only product-owned home/path labels change. |
| WI-06 | Nested discovery | Successful settled native file read/write/edit operations discover instruction candidates along descendant directories from session cwd to the touched file's parent. Absolute and relative paths work. Root-level touches still refresh previously loaded scopes. Do not recursively scan the whole repository. |
| WI-07 | Refresh and retirement | A relevant successful touch rechecks newly reached and previously loaded scopes, including global instructions. Add new files, replace changed content, and append explicit removal notices for confirmed absence or duplicates. Reappearing files load again after a removal tombstone. |
| WI-08 | Correct timing and ordering | Baseline follows the claimed human prompt before the first model dispatch. New context follows completed tool exchanges and their committed step boundary. Coalesce pending changes per Agent. Rejected or empty initial steps retain pending context without generating their own request. |
| WI-09 | Nested/composite execution | Use trusted execution identity and parent/child relationships to collect file touches. Successful child file operations remain observable even if a later composite operation fails. Publish only after the enclosing result/step settles. Failed, agentless, blocked, or already-cancelled individual operations do not invent successful touches. |
| WI-10 | Bounded reads and rendering | Match the reference 1 MiB source-file default and 64 KiB rendered-batch default. Enforce source caps during streaming even when size metadata is absent or stale. Retain the most-specific suffix of instruction files, omit broader files first, then truncate the most-specific file on a UTF-8 boundary. Name omissions/truncations visibly. |
| WI-11 | Honest partial state | A file is considered represented only when its own semantic section includes content, or its genuinely empty-file section survives. Do not mark notice-only or entirely omitted content as loaded. Budget-omitted dynamic changes remain eligible for retry. Partial content retains its complete-content digest as in the reference. |
| WI-12 | Provider and filesystem semantics | Read through the approved filesystem abstraction, including provider-visible files; never fall back silently to host reads. Follow file symlinks where the existing authority permits their target. Missing/non-file targets are absence; probe/read failures and oversized files are temporary unavailability, not evidence of deletion. An unavailable sibling does not suppress other candidates. |
| WI-13 | Cancellation and caching | Propagate cancellation through resolution, stat, and streaming. Once an accepted result's touch is queued, its projection uses the Agent lifecycle cancellation scope rather than a completed tool's disposable signal. Keep caches isolated by Agent/session; use provider versions only as an optimization and content hashes for identity. No process-global instruction-text cache. |
| WI-14 | Resume and reload | Reconstruct effective instructions from existing durable context messages and metadata. Reuse compatible visible baselines; append offline additions, edits, and removals. Explicitly supersede an incompatible baseline, including the empty case. Recover pending-but-unadmitted context without duplicating it. |
| WI-15 | Compaction recovery | Implement the visible-context contract, restoration algorithm, and synthetic-compaction tests now. A baseline removed from model-visible history is restored before the next admitted model request even without another file touch. Nested scopes are re-armed when their instruction messages leave the visible context. Section 5 defines the stronger Aworkit integration guarantee for already active scopes. |
| WI-16 | Node/branch/child isolation | Loader state belongs to the Chat, Agent node, and branch/child context. Different nodes and subagents cannot contaminate each other's loaded scopes. Explicit context inheritance follows frozen workflow selection. A selected child plugin runs against the child's authorized cwd; it does not widen authority. |
| WI-17 | Diagnostics and inspection | Run details identify loaded/updated/removed files, scope, and omissions or temporary read failures. Reuse existing history and diagnostic presentation. Do not scatter repeated notices across Chat or create a new provenance UI. Settings controls have labels and tooltips. |
| WI-18 | No provider dependency | If no authorized filesystem provider is available, the plugin is inactive for that preparation and does not prevent the application from booting. Record the unavailable diagnostic, retry at an eligible boundary, and never fabricate an empty removal update. |

Reference limitations deliberately retained: refresh is driven by structured successful file activity, resume, or visible-context restoration; there is no watcher. Shell `cd`, shell/Python text, search/glob results, arbitrary MCP output, or model-written prose do not count as trusted file-touch events. Lowercase candidate aliases, `.claude/rules/`, and `@path` imports are not inferred. A nested rule discovered after a write applies to the next model request; pre-write discovery is a separate possible enhancement, not claimed parity.

## 3. Configuration

| Field | Default | Meaning |
|---|---|---|
| `aworkitHome` | OS home + `.aworkit` | Global instruction home; resolve and freeze as an absolute path. Custom home labels identify the actual Aworkit setting. |
| `projectRootMarkers` | `[".git"]` | Root discovery markers within permitted workspace scope. |
| `instructionFileCandidates` | `["AGENTS.md", "CLAUDE.md"]` | Ordered project base candidates. |
| `localInstructionFileCandidates` | `["AGENTS.local.md", "CLAUDE.local.md"]` | Ordered project overlays; `[]` disables them. |
| `maxBytes` | `65536` | UTF-8 bytes in a complete baseline/update/restoration batch, including framing and diagnostics. Zero disables injection. |
| `maxSourceBytes` | `1048576` | Maximum bytes streamed from one candidate; positive integer. |

Candidate entries are same-directory file names; ignore empty names, `.`/`..`, and names containing slash or backslash as the reference does, and show a Settings diagnostic for unusable entries. Persisted numeric Settings use validated finite integer bounds; the low-level renderer must still handle disabled/invalid render budgets defensively. The reference's non-positive/non-finite disabled behavior must never lead to unbounded reads or rendering.

Resolve workspace scope through Aworkit's existing frozen workspace binding. Detect meaningful root/candidate/order/budget identity changes and use a superseding baseline; do not silently reinterpret old relative paths under a different root. Instruction-root discovery is not permission to read outside approved project/global locations. A reference symlink target outside approved roots is reported unavailable under Aworkit's existing authority policy, rather than granting host access. This is an explicit product authority adaptation.

## 4. Lightweight persistence and runtime boundaries

Use the existing Chat/Run event store and context projection. No separate memory database, file-version archive, or parallel audit system is required.

An instruction context event stores its exact rendered message plus small typed metadata: producer identity, owner (Chat/node/branch/child), baseline flag and compatibility identity, and changes `{action: set|replace|remove, scope, path, digest?}`. Derive scope from directory plus candidate filename, so siblings do not collide. Use the reference SHA-1 content identity for parity; it is a change-detection fingerprint, not a security attestation. Pending/admitted event identity comes from existing runtime records. Provider version and trimmed-content hash are a rebuildable session cache, not a new durable version model.

The effective-state index is reconstructed from these records, including removals and explicitly superseded baselines. Cache hits cannot establish model visibility. Only a committed and selected instruction event can establish what a request received. Repository text, headings, tool output, or a compactor's prose cannot forge typed loader metadata.

There are two different operations:

1. **Replay/retry an already prepared request:** reuse the exact recorded context and completed tool outcomes. Do not reread files to rewrite that request.
2. **Prepare a new request after resume or a file touch:** compare current files with the effective visible state and append any needed transitions. A transient read failure keeps previously visible guidance; confirmed deletion produces retirement.

Implement focused modules rather than expanding `tool_loop.rs` into a loader:

| Responsibility | Intended home |
|---|---|
| Config normalization, discovery, bounded provider reads, deduplication, and deterministic prompt rendering | `crates/aworkit-capability-host/src/workspace_instructions/`, using existing approved filesystem access |
| Per-Agent lifecycle, pending changes, visible-state reconciliation, and projection planning | Logical component `aworkit.workflow_worker.workspace_instructions`; current desktop composition in a dedicated `runtime/workspace_instructions/` module |
| Canonical message/metadata persistence and request projection | Existing trusted-core history commit and `aworkit.workflow_worker.context_store` boundaries |
| Plugin contribution kind, validation, availability, frozen selection, and configuration | Existing manifest registry, Settings, snapshot freezer, and workflow tool selector |

Route automatic preparation through a common Agent request boundary for both plain text requests and model/tool loops. The current `ModelToolInvocationPortV1::prepare_context` and Skills event recording are reuse points, not a complete solution: Agents with no callable tools currently take a separate path, and `ChatHistory::conversation()` currently selects only ordinary user/assistant messages. Extend the real projection so instruction context persists across later Chat inputs as well as approval resume, restart, and tool continuations. Do not merely re-inject everything for each new outer invocation.

A request may proceed only after its planned context and related events have been durably admitted. Storage failure or cancellation must not mark an uncommitted file version as visible. Keep queues serialized per owner, coalesce obsolete pending updates, and keep settled file results adjacent to their tool calls in provider protocols.

Plugin disposal unregisters lifecycle listeners, cancels its pending probes, and releases per-owner queues/caches. Remount reconstructs from admitted history instead of resetting the model's effective state. A source metadata version is optional: a cold baseline must read and hash content, including same-size/same-timestamp rewrites; dynamic fast-path reads may be skipped only with a trustworthy unchanged provider version and matching visible state.

## 5. Compaction integration is a required deliverable

The plugin's compaction support must be implemented and tested with the loader. It must not be left as a TODO. The summarization engine and production compaction trigger are separate future work because Aworkit does not have them today.

### Contract to implement now

Expose one preparation boundary receiving:

- Frozen plugin configuration, owner, authorized workspace/cwd, and cancellation.
- The actual model-visible context selection and a stable revision/generation identity, including authentic instruction event references.
- Newly settled structured file touches, plus first-request/resume/context-replaced lifecycle reasons.
- Existing durable loader records needed to reconstruct active scopes and retirements.

It returns a bounded context delta, the existing event IDs to retain/reference, and diagnostics. The current non-compacting runtime supplies its ordinary visible selection. A future compactor must supply its replacement selection through the same boundary. A callback alone is insufficient: reconciliation must inspect what survived in the actual context.

### Restoration rules

1. Keep durable history separate from the smaller model-visible projection. Compaction changes the latter, not past request records.
2. If an instruction event remains fully represented, retain it without duplication. A summary that mentions the rules is not proof that the instruction event survived.
3. If the baseline is missing, rebuild and admit it before the next model request, even when no new file tool has run. In-memory metadata/version cache hits do not suppress restoration.
4. Keep discovered active scope identities recoverable independently of the compacted text. Retired or superseded scopes remain retired; a summary must not resurrect them. Newly reappearing files still qualify for a new addition.
5. Restore currently applicable, previously active nested instructions along with the baseline, subject to the shared restoration budget, before the next request. This is an explicit Aworkit integration guarantee beyond the reference's minimum of re-arming removed nested scopes for the next relevant file touch. It avoids losing an already active nested rule while the model is deciding its next operation.
6. Reconcile current file contents during new post-compaction preparation. If a previously active file is temporarily unavailable, its last admitted bounded text can be restored from ordinary history with an explicit stale/unavailable diagnostic; do not claim a fresh read. Confirmed absence removes it. This fallback uses already persisted messages, not a second file archive.
7. Omitted/truncated restoration is visible, remains bounded, and does not falsely mark absent instruction content as present. Do not make a model summarizer rewrite the original rules as part of loader restoration.
8. Publication is atomic relative to request admission. Cancelled or rejected compaction/preparation leaves the last committed context usable. Repeated restore calls for the same owner and context revision are idempotent; successive compactions can re-arm restoration again.

The complete restoration batch shares the 64 KiB default budget. Baseline ordering follows the reference; additional active nested scopes retain directory-qualified text and deterministic depth/candidate ordering. Preserve more-specific content first and name anything omitted. Multiple sibling directories remain scoped; one sibling's guidance is not promoted to a global rule.

### Required tests now and future integration gate

Use the production context preparation boundary with an in-memory test projection that removes or replaces instruction events as a compactor would. Verify first-request restoration, unchanged files with warm caches, changed/deleted/offline files, active nested scopes, removal tombstones, partial survival, repeated compaction, budget pressure, cancellation, restart after projection replacement, and parallel owner isolation. A loopback provider fixture must verify the exact restored instruction messages are present in the first request after simulated compaction.

The future compactor cannot be considered integrated until these same cases run through its real context-replacement path. Until then report **loader compaction recovery prepared and tested; automatic conversation compaction not yet implemented**. This integration gate is part of this specification and the formal UML contract, so it cannot be silently dropped from the later compactor implementation.

## 6. Prompt compatibility

Port `src/render.ts` templates and budget behavior with golden fixtures. Keep its original wording for the following cases, changing only DeepSeek-owned home/path labels to Aworkit:

- Baseline: `The following workspace instructions may be relevant to your work. Use them as guidance when applicable. More specific instructions take precedence over broader ones. They do not override system, developer, or direct user instructions.` Then `Instructions from: <path>` and original file content.
- Addition: `Additional instructions from: <path>` and `These instructions apply to work under ...` with the reference scope/precedence wording.
- Replacement: `Updated instructions from: <path>` and `This file changed after it was loaded. Use the following content instead of the previously loaded instructions from this file.`
- Removal: `Instructions removed: <path>` and `The previously loaded instructions from this file no longer apply.`
- Superseding baseline: `This complete workspace instruction baseline replaces all earlier workspace instruction baselines.` Include `No workspace instructions are currently active.` for an empty replacement.

Keep messages in the user role and preserve tool-call/result ordering on OpenAI-compatible, Anthropic, and Gemini adapters. Escape literal closing `</system-reminder>` delimiters in content, paths, scopes, and budget diagnostics using the reference escape. Do not add a second provider/core wrapper or parse Markdown content as loader state.

## 7. Acceptance and implementation sequence

1. **Plugin integration:** add the automatic contribution kind across manifest parsing, Settings controls/tooltips, enablement, Agent selection, frontend/native validation, installed executor admission, freeze, and model capability checks. Test ordinary callable tools remain unchanged, and an instruction-only Agent runs through the text path.
2. **Reference port:** implement config/files/render/state modules and a behavior matrix against the reference test inventory. Cover all WI requirements, including candidate ordering, trimmed duplicates, symlinks under authority, read failures, unknown size, same-size rewrites, UTF-8 limits, and empty-file semantics.
3. **Lifecycle and persistence:** wire initial preparation, all relevant settled native file operations, queued/coalesced changes, nested execution tokens where supported, cross-input Chat projection, approval resume, restart, child isolation, and commit-before-dispatch behavior. Test zero duplicate messages and zero repeated effects on retry.
4. **Compaction readiness:** implement Section 5's visible-context contract and production-boundary tests now. Retain a reusable integration suite for the future compactor. Do not stub the restore algorithm or claim the engine exists.
5. **Native end-to-end proof:** in an isolated profile, enable the plugin in Settings, select it on an Agent, Validate, Save, and Run. Verify global/project/nested instructions, mutations/removals, disabled behavior, a second Chat input, restart/resume, an instruction-only Agent, and simulated compaction on provider-bound messages. Rebuild frontend and native executable first. UI proof must exercise the real editor, not only pre-bound backend state.

Completion requires evidence for each WI row. Any provider/platform/authority limitation is reported explicitly and linked to its test; unimplemented behavior is not described as parity. This work implements workspace instructions, not a memory-writing API, a second Skills tool, a compaction engine, or unrelated Harness features.

## 8. Source and architecture traceability

- Harness: `packages/context/agent-instructions/src/{index,config,files,render,state,digest}.ts`; `tests/agent-instructions.spec.ts` and `tests/agent-instructions.e2e.ts`; base bundle `maxBytes: 65536` registration.
- Current Aworkit integration points: `desktop/tool-plugins/aworkit-native/tool-plugin.json`, `desktop/src/workbench/{toolRegistry,workflowExecution,NodeConfigurationForm}.ts(x)`, `desktop/src-tauri/src/runtime/{tool_registry,settings_v2,history,graph_pass,model_tool_loop}.rs`, `runtime/tool_loop/skills.rs`, and capability-host model context/provider adapters. Source is being refactored concurrently; follow symbols to their current modules at implementation time.
- Formal component: `aworkit.workflow_worker.workspace_instructions`, cooperating with `aworkit.workflow_worker.context_store`, `aworkit.capability_host.tool_runtime`, `aworkit.trusted_core.snapshot_freezer`, and desktop Settings/workflow editor.
- Formal UML: `uml.workflow_worker.workspace_instructions_activation`, `uml.workflow_worker.workspace_instructions_lifecycle`, and `uml.workflow_worker.workspace_instructions_compaction`.
