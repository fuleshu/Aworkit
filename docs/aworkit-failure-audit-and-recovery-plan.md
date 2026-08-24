# Aworkit failure audit and specification recovery plan

Date: 2026-08-23  
Original implementation baseline: Git commit `b62c5a1`  
Product specification: `aworkit_design_concept_final.md`  
Reference implementation inspected:

- `chatshellapp/chatshell-desktop` at `63aab8d2537e9f0c02e06a0878032bf3c6974282`
- `chatshellapp/chatshell-agent` at `c4e1e170394648a5936899ee0fb29854027979a7`

This document distinguishes the original failed delivery from the recovery work in the
current working tree. A type, a mock, a Settings card, or a green screenshot test is not
counted as a working feature. A feature is working only when the visible desktop control
reaches its native production adapter, produces the intended effect, commits truthful
evidence, and survives a new application process where persistence is part of the feature.

## Executive verdict

The original desktop was a nonfunctional product shell over a substantial but
uncomposed framework. Its automated suites passed because they tested horizontal pieces
and visual projections, not the user outcome. The production desktop explicitly booted
demo state, fabricated successful activity, and stopped first-send handling after adding
the user's message. It never invoked a configured model and never produced a real
assistant answer.

Responsibility is shared, but not equally:

1. **The design concept was too broad to be a release plan.** It lacked a staged delivery
   contract and a binary native acceptance gate. It nevertheless clearly required
   functional providers, models, tools, MCP, workflows, durable Chats/Runs, projects, and
   Settings. It did not authorize placeholders represented as complete.
2. **The implementation tasks were badly decomposed and badly closed.** They rewarded
   contracts, reducers, and mocked layers independently. No owner was accountable for a
   complete Settings → frozen workflow → authority → provider/tool → durable history path.
3. **The implementation failed at production composition.** Useful core, store, worker,
   and host primitives existed, but the desktop executable did not connect them. Demo
   projections and constant capability labels were wired into the shipped path instead.
4. **Review and QA used the wrong proof.** Preview adapters, fake providers, screenshot
   geometry, and package manifests were accepted where native effects, restart behavior,
   and failure paths were required.
5. **The ChatShell instruction was misread.** “Reference, not a fork” was treated as “do
   not inspect the reference.” That discarded working interaction and integration evidence
   for provider/model setup, tools, and MCP without protecting Aworkit's architecture in
   any useful way.

The failed result was therefore not principally caused by visual design. It was caused by
false completion semantics.

## What the original specification actually required

The final concept is explicit about the relevant product behavior:

- configured providers, concrete models, credentials, tools, MCP servers, extensions,
  projects, external agents, data behavior, and appearance are managed in the app;
- ChatShell Desktop is the main reference for the desktop shell, provider/model setup,
  MCP configuration, Settings, themes, and similar infrastructure;
- a Chat is one persistent Run whose workflow, model resolution, tools, project workspace,
  and authority are frozen at first input;
- the built-in Simple Chat graph is Input → Agent → Output → Wait for Input;
- the Agent may run a bounded model/tool cycle;
- direct project tools must enforce the selected root rather than merely set a working
  directory;
- Codex App Server over local standard input/output is the first rich external-agent
  integration target;
- canonical configuration is secret-free JSON, secrets stay in the operating-system
  credential store, and ordinary local semantic history is durable SQLite;
- old history must never replay a model call, tool action, or other side effect; and
- missing or incompatible dependencies must remain visible and losslessly editable rather
  than being silently removed or reported as ready.

The concept's real defect is scope and sequencing. It describes a product family, not a
first release. It should have been followed by a delivery specification naming the first
executable graph, supported node subset, supported providers/tools, platform scope, and
native restart gate. The absence of that second document explains planning drift; it does
not excuse shipping a mock UI.

## Original task-plan audit

The original task system made four structural mistakes.

### Horizontal work was called product completion

Examples from the recorded task set:

- “Implement the model and provider gateway” delivered provider-neutral interfaces but no
  concrete desktop provider.
- The Settings task named providers, tiers, credentials, tools, extensions, MCP, agents,
  data, projects, and appearance, while its evidence covered a small number of frontend
  draft and eligibility helpers.
- Worker and provider tasks explicitly used fake capability outcomes and fake providers.
  Those are appropriate unit fixtures, but no later task proved that production composition
  replaced them.
- The Milestone 08 “desktop vertical slice” asserted frontend tests, Rust tests, browser
  interactions, package assembly, and a native screenshot. It did not assert a provider
  request, assistant response, credential operation, tool effect, or restart continuation.

### Definitions of done measured artifacts, not outcomes

“A contract exists,” “the screen renders,” “the package contains the binary,” and “the
mock returns success” were treated as equivalent to usable behavior. They are not. Every
cross-layer feature needs one acceptance owner and one effect-oriented gate.

### Bulk status closure bypassed independent review

Tasks 44 through 50 were marked finished within nine seconds on 2026-08-21. In the audited
database, 77 tasks were `finished`, one was `confirmed`, and none of the 78 rows had a
confirmation commit. That is status bookkeeping, not verified delivery.

### No task enforced the obvious user journey

The plan omitted this indivisible release task:

```text
fresh profile
  → create a provider and model
  → store a credential without exposing it to the renderer
  → discover/test the exact provider draft
  → map tier:balanced
  → select Simple Chat and an optional project
  → send first input
  → freeze workflow/model/project/authority
  → receive and commit a real assistant response
  → terminate the process
  → reopen the same profile and restore the same logical Chat/Run
  → send a follow-up with prior context
  → do not replay any prior external effect
```

Without that gate, every team could finish its layer while the application still did
nothing.

## Original implementation audit

At baseline `b62c5a1`, the failure was concrete:

- `desktop/src-tauri/src/main.rs` registered `DesktopRuntime::default()`.
- `DesktopRuntime::default()` called `Self::demo()`.
- The demo constructor injected fake Settings, projects, Chat state, and timeline entries.
- The desktop fabricated a successful `cargo test`, token usage, evidence, artifacts, and a
  Repository Engineer workflow.
- First send changed aggregate state and appended a user message, but invoked no worker,
  capability host, provider, tool, or assistant-output path.
- Settings used a constant capability inventory. Credential replacement was disabled and
  Projects were read-only.
- Workflow UI actions could manufacture a `ready` label; native validation checked little
  more than the existence of node and edge arrays.
- The packaged sidecar binaries implemented smoke handshakes, but ordinary desktop
  commands did not compose them into a workflow.

The underlying framework was not uniformly fake. It contained serious protocol,
authority, storage, scheduling, process, and recovery primitives. The failure was that
those primitives were not composed into the desktop product, while simulated output hid
that fact.

## Why green tests allowed the failure

| Gate | It proved | It did not prove |
|---|---|---|
| Core/domain unit tests | lifecycle legality, hashes, authority and persistence primitives | the desktop invoked them as one operation |
| Provider/host tests | neutral seams using fakes | an installed provider received a request |
| React/Vitest | reducers, drafts, static projections and accessibility | Tauri IPC produced a real native effect |
| Browser visual QA | Preview layout and interaction geometry | native state, credentials, processes or network effects |
| Native screenshot smoke | a Tauri window rendered plausible pixels | any control completed its stated operation |
| Release assembly | expected files were packaged | the package delivered a usable workflow |

The browser integration test rendered `App` outside Tauri and therefore selected Preview
ports. The old native gate asserted pixels and one queued user message. Both could be green
while the product remained nonfunctional.

## How ChatShell is being used correctly

The checked-out ChatShell sources are a behavioral reference, not Aworkit's architecture.
The detailed file-level audit is in `docs/chatshell-reference-audit.md`.

Useful patterns reimplemented independently include:

- a provider catalog and custom-provider path;
- distinct provider/model identities, model CRUD and discovery;
- exact connection-test feedback and visible errors;
- a clear built-in tool inventory with per-tool configuration and health;
- MCP HTTP/STDIO configuration, lifecycle state, discovery, and capability presentation;
- accessible action names, confirmations, draft preservation, and theme controls.

ChatShell behavior deliberately not copied includes decrypted credentials in renderer
state, weakly atomic multi-step saves, plaintext integration secrets, implicit activation,
working-directory-only file safety, and ChatShell-specific product/state architecture.
Aworkit retains its canonical workflow graph, tier routing, frozen Chat/Run, trusted-core
authority, opaque credential references, root-confined tools, semantic history, and
external-agent distinction.

## Full specification compliance matrix

“Current recovery” means production code in this working tree, not the original baseline.
“Closed” requires the final gates listed later in this document.

| Concept area | Original result | Current recovery | Remaining closure condition |
|---|---|---|---|
| Desktop shell and progressive Settings | Plausible shell over demo/constant state; controls disabled or inert | Demo records removed; all ten Settings v2 sections are native projections. Implemented controls have native command paths and visible operation state; retention/portable/auto-connect and uncomposed execution controls are explicitly disabled instead of being accepted and ignored | Complete the pending browser and packaged-native interaction/restart gates; add cross-platform ports |
| One Chat = one Run | First send stored only the user message | Unique logical Chat/Run identity, first-send freeze, provider-backed response, follow-up, and restart reconstruction. First and follow-up effect commands are staged durably before provider execution. A recovered pending command pauses the Chat behind explicit **Resume** and confirmed **Abandon as uncertain** actions; ordinary send, New Chat, replacement, and cancel paths cannot bypass it. Resume uses the exact stored command and broker identity, while abandon performs no provider/tool call and records an uncertain outcome with automatic replay forbidden | Complete the packaged-native kill/reopen proof; add multi-Chat navigation |
| Direct project workspaces | Fabricated projects; no CRUD or execution binding | Settings project CRUD/probe plus pre-Chat project selection, canonical frozen workspace binding, root identity and optional Git branch revalidation before proposal/dispatch and each read/search call, and no-project pre-provider denial | Pass the pending packaged-native selected-project tool gate; remote workspace adapter remains absent |
| Workflow JSON/editor | Permissive validation and fake readiness | Lossless schema-v1 graph editor with import, node/edge editing, drag, undo/redo, validation, inspect/export/save; future schemas remain read-only and lossless | Add executors one node family at a time; arbitrary valid graphs are not yet runnable |
| Built-in Simple Chat | No provider execution | The exact saved Input → Agent → Output → Wait graph is compiled into the Scheduler execution plan and traversed as Input claim/ack → Agent → Output → suspended Wait. Checkpoints and ordered traces are persisted for success and failure. Frozen budgets are settled from actual attempted model turns and settled tool calls rather than fabricated constants | Pass the pending packaged-native project/tool restart gate; streaming and in-flight pause/cancel remain separate scope |
| Frozen graph and dynamic execution | Worker primitives only | Workflow/settings/model/provider/credential, selected workspace, Agent limits, optional instructions, and permitted tools are frozen for the logical Chat/Run. The supported Simple Chat graph runs through the Scheduler with persisted checkpoints/traces and explicit Wait suspension | General routing, joins, approval nodes, retries, and other suspension types are not composed |
| Harness Context | Domain primitives only | Persisted user/assistant conversation becomes the next provider request. Optional saved Agent `instructions` are validated as a non-empty, NUL-free value of at most 64 KiB and supplied as the exact leading system message. Input/Output/Wait configuration must be empty, and Agent accepts only `modelTierId`, `toolIds`, `maxTurns`, and optional `instructions`; unknown configuration is rejected before freeze | Layered context selection, transformations, and source inspector remain absent |
| Providers and concrete models | No concrete desktop provider | OpenAI-compatible, Anthropic Messages and Gemini adapters; provider/model CRUD, discovery and tests | Provider-specific parameter allowlists and complete native gates for all three |
| Portable model tiers | Labels/contracts without desktop effect | Four standard tiers and custom tiers persist with Exact/Fallback/Policy editors; Simple Chat freezes and executes only an Exact `tier:balanced` mapping, while other strategies now block before send with an actionable reason | Implement and prove ordered fallback and policy semantics end to end; they are not silently flattened |
| Credentials | Disabled control / no usable path | Dedicated write-only create/replace/clear operations; opaque references and metadata in JSON; scoped lease materialization. A separate versioned, secret-free credential-operation journal records replacement/deletion intent before keyring effects, reconciles it against saved Settings and active frozen bindings at startup, retries cleanup without blocking profile open, and exposes unresolved cleanup as provider-health warnings | Disposable real-keyring crash/reopen CI on Linux, Windows, and macOS |
| Built-in tools | Constant inventory, no execution | Five tools have canonical settings and real diagnostics. Only project read/search are bindable to the current Simple Chat; edit/shell/Python remain visibly disabled. Generic Settings cannot turn those unsupported tools from `false` to `true`, while preexisting legacy `true` metadata remains lossless and may be cleared. Enabled read/search execute provider → authority broker → authenticated Capability Host → root-confined Project Files → provider continuation with durable settlement and visible activity. The shared contract bounds reads to 65,536 bytes and searches to 512 results; aggregate message context is limited to 256 KiB, assistant text to 16 KiB, and each durable model/tool exchange to 512 KiB. Request/context overflow is rejected before staging, and tool/outcome overflow is rejected before durable commit | Pass the pending packaged-native gate; approval UI and workflow execution are still required before edit/shell/Python may be bound |
| MCP | Label only | Durable secret-safe HTTP/STDIO configuration plus real protocol initialization/discovery/probe. MCP execution is explicitly presented as unavailable rather than bindable. Generic Settings rejects `enabled: false → true`, preserves preexisting legacy `true` metadata, and permits clearing it | Compose selected MCP calls into a frozen workflow/Agent loop; apply auto-connect lifecycle honestly |
| External agents | Label only | Durable target configuration and a bounded Codex App Server initialization/account/model capability probe. External-agent execution is explicitly unavailable. Generic Settings rejects `enabled: false → true`, preserves preexisting legacy `true` metadata, and permits clearing it | External Agent node start/progress/continuation/cancel/approval execution is not yet composed |
| Trusted extensions | Registry primitives only | Manifest inspection, separate versioned registration, and post-registration trust metadata; exact entrypoint identity/hash is revalidated. Registration remains disabled and extension enablement/execution are explicitly unavailable. Generic Settings rejects `enabled: false → true`, preserves preexisting legacy `true` metadata losslessly, and permits clearing it to `false` | Add an explicit enable operation only together with plugin-host launch/handshake/dispatch, contributed-capability inventory, and dependency inventory |
| Workflow authority | Useful primitives defeated by fake readiness | Saved references are resolved before execution; snapshot/manifest, broker, authenticated capability host, scoped secret lease, Scheduler checkpoints/traces, actual model/tool budget settlement, and durable invocation settlement are used | Each new tool/MCP/external operation needs its own binding, budget, approval, and no-replay gate |
| Transparency/evidence | Fabricated commands, usage and success | Provider identity/model/tier, usage, snapshot, manifest, invocation, outcome hash, actual model-turn/tool-call counts, and failures are factual. The UI resolves each activity card through the exact `evidence.<timelineId>` key, preventing one card from borrowing another event's evidence | MCP/external activity cards and a complete evidence inspector remain incomplete |
| Local configuration/history | In-memory demo composition despite real store code | Secret-free versioned JSON settings/workflow documents and durable SQLite semantic Chat history | Current physical projection is still one local stream; migration/corruption UI needs native tests |
| Data and portable sessions | No functional desktop behavior | Local SQLite is truthfully identified as the only active backend. Portable sessions, detailed capture, retention and per-project portable history are visible but disabled; legacy active flags are normalized off once rather than ignored | Implement retention/capture behavior and exclusive local-vs-portable Chat placement before enabling these controls |
| Management Chat/repair | Fabricated repair activity | Fabricated state removed and the unavailable surface is explicit | Full management workflow and activation/restart/rollback path remain unimplemented |
| Multi-process architecture | Sidecar smoke binaries, no normal workflow composition | Authority, broker, capability host and provider adapters are composed in the native service, currently in-process | Move the same proven path across supervised process boundaries without weakening crash semantics |
| Platforms | Linux screenshot only | Rust and frontend suites are green; browser and Linux packaged-native behavioral gates remain pending and are not counted as proof yet | Pass the pending browser/Linux packaged-native gates, then add equivalent Windows and macOS native/keyring/package gates |

This matrix intentionally does not turn configured or probed integrations into claims of
workflow execution. Configuration is a real feature; execution is a separate feature and
needs separate proof.

## Corrected implementation plan

The repair is organized as vertical outcomes. A phase cannot close from mocks, screenshots,
types, or separately green layers.

### Gate A — honest Settings foundation

1. One canonical, version-checked, secret-free Settings v2 document.
2. Native CRUD and validation for providers/models, tiers, tools, extensions, MCP,
   external agents, data, projects, and appearance.
3. Credentials only through dedicated write-only native operations, with each keyring
   replacement/deletion protected by the versioned credential-operation journal and
   reconciled at startup without placing secret material in durable documents.
4. Exact-draft provider test/discovery, MCP probe, external-agent probe, project probe,
   extension inspect/register, and built-in-tool diagnostics.
5. Every failed operation preserves the draft and reports an actionable error.
6. Close/reopen reconstructs the saved non-secret state.

### Gate B — one complete specification-aligned Simple Chat

1. Select the saved Simple Chat workflow and No project or one eligible saved project.
2. Resolve `tier:balanced` to a supported enabled provider/model.
3. Validate the exact supported node configuration; on first send, freeze the complete
   workflow, optional Agent instructions, model resolution, credential metadata,
   project/workspace and Git branch, allowed tools, limits, and authority.
4. Stage first and follow-up effect commands durably, then execute provider requests
   through snapshot → proposal → durable broker → authenticated capability host → scoped
   credential lease → concrete provider.
5. Traverse Input → Agent → Output → Wait through the Scheduler, persist checkpoints and
   traces, suspend at Wait, and settle the frozen budget with actual attempted model turns
   and settled tool calls.
6. If no tools are bound, commit the bounded assistant answer and usage.
7. If root-confined read/search tools are bound, accept only normalized calls matching the
   frozen definitions, revalidate root identity and optional Git branch, enforce the shared
   read/search/context/exchange bounds, return settled results to the same provider loop,
   and commit truthful tool evidence plus the final answer.
8. Terminate the application process, reopen the profile, show the same Chat/Run, and send
   a contextual follow-up without replaying any earlier effect. If a staged command has no
   terminal Chat evidence, pause behind explicit Resume/Abandon recovery: Resume reuses the
   exact command and durable identities; Abandon performs no external effect and records an
   uncertain outcome with automatic replay forbidden.

### Gate C — effectful built-in tools

1. Add an authority preview before first send.
2. Add an explicit approval path for file edit, Host Shell, and Host Python as required by
   their saved configuration.
3. Execute each call as its own durable, bounded, cancellable authority operation.
4. Record exact command/path/effect/exit/approval facts without secrets.
5. Prove rejection outside the project root, stale root identity, timeout, cancellation,
   uncertain outcome, restart, and no silent retry.

### Gate D — MCP workflow execution

1. Freeze one enabled/probed server identity, transport, discovered capability identity,
   credential bindings, and generation into a workflow.
2. Execute one resource/tool/prompt call through the MCP session and authority broker.
3. Project connection state, progress, result and failure into semantic history.
4. Prove STDIO process-tree cleanup, HTTP bounds, cancellation, changed-server isolation,
   restart, and no implicit authority expansion.

### Gate E — external agents and extensions

1. Implement the explicit External Agent node using the Codex App Server adapter, including
   declared lifecycle capabilities and native session identity.
2. Implement an explicit extension enable operation together with registered-extension
   process launch, handshake, contributed capability inventory, crash cleanup, and frozen
   version/hash dispatch; registration or trust metadata alone must never imply enablement.
3. Keep MCP tools, ordinary models, and external-agent lifecycles distinct.

### Gate F — breadth and platform release

Add routing, subagents, additional graph executors, multi-Chat/fork/retry, portable
sessions, schedules, Management Chat/repair, and Windows/macOS gates one complete vertical
slice at a time. None should be enabled merely because a domain contract already exists.

## Mandatory native acceptance tests

The minimum release gate for the current recovery is:

```text
fresh native profile
  → configure/discover/test provider and model
  → map tier:balanced and save Settings v2
  → edit/save/reload the exact Simple Chat graph
  → choose project scope if testing project tools
  → send first input through real Tauri IPC
  → observe the real provider/tool effects and committed assistant output
  → hard-stop the desktop process
  → reopen only from the same persisted profile
  → recover the same logical Chat/Run and frozen identities
  → send a follow-up carrying prior context
  → assert exact effect counts and zero replay of committed calls
```

Supplementary mandatory gates are:

- full Rust workspace tests and warning-denied Clippy;
- full frontend tests, TypeScript check, and production build;
- a browser Preview gate that explicitly refuses to count Preview Chat/native effects;
- malformed, timeout, disabled, missing, changed-draft, stale-version, scope-escape,
  credential-redaction, and restart failure cases; and
- `git diff --check`.

## Verified recovery results

These are the source-level gates already verified for the current recovery:

| Gate | Result |
|---|---|
| Desktop Rust library tests with warnings denied | 108 passed, 0 failed |
| Workflow worker tests | 21 passed, 0 failed |
| Frontend tests | 98 passed, 0 failed |
| Frontend typecheck and production build | Passed |
| Rust formatting and warning-denied Clippy | Passed |

The browser Preview gate and both packaged-native gates—the real-provider Simple Chat
restart path and the selected-project read/search tool path—are still pending. They are not
counted as release evidence or as closure of this audit until they pass against the frozen
source.

## New definition of done

A visible feature is complete only when all of these are true:

1. The control invokes its native production operation, not a constant or Preview effect.
2. The intended effect occurs and its truthful result is visible.
3. The operation is constrained by the saved/frozen configuration and authority.
4. Canonical state survives a fresh process where durability is part of the feature.
5. Committed side effects are not replayed; ambiguous outcomes are reported, not guessed.
6. Error and version-conflict paths preserve user input and give a repair route.
7. Secrets never enter canonical JSON, projections, logs, screenshots, receipts, or test
   snapshots.
8. A deterministic native end-to-end test crosses every production boundary used by the
   shipped path.
9. The release job runs that gate. Unit, visual, and packaging tests supplement it; they do
   not replace it.
10. Any remaining limitation is visible in the UI and in this audit, not hidden behind a
    `ready` badge.

## Bottom line

The concept needs release slicing, but it was not the reason the screen did nothing. The
task plan allowed components to be declared done without composition; the implementation
then shipped demo state; QA certified appearance and mocks instead of effects; and the
ChatShell reference was not inspected. The recovery keeps Aworkit's specification and
architecture, uses ChatShell only for proven interaction patterns, and makes native,
restart-safe, authority-checked user outcomes the only acceptable completion evidence.
