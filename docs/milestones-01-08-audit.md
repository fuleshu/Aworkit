# Milestones 01–08 implementation and QA audit

Audit date: 2026-08-21  
Audited scope: Adashi tasks #5–#55 and QA jobs J1–J8  
Final status: remediated; consolidated Adashi QA run group #24 passed all eight scoped jobs

## Finding and conclusion

The initial completion claim was not supportable. The desktop shown to the user was the Milestone 01 placeholder from a stale native binary, while the then-current Milestone 07/08 jobs exercised only unit tests, TypeScript, and a Vite build. They did not launch the Tauri/WebKit application or compare a rendered surface with the accepted desktop geometry. Earlier jobs in Milestones 01–06 were similarly too shallow or stale: important boundary, protocol, persistence, real-process, crash-recovery, authority, and portable-history contracts had no executable evidence. A finished task flag and a historical green job therefore did not mean the milestone was genuinely complete.

The implementation and runners were subsequently reviewed contract by contract and remediated. The evidence tables below tie every task to concrete source and a final passing QA run. This conclusion is deliberately limited to the published contracts of Milestones 01–08; it is not a claim that the complete Aworkit product roadmap has shipped.

## Material gaps fixed

| Milestone | Initial material gap | Remediation now evidenced |
|---|---|---|
| M01 | Placeholder-only native proof; weak dependency checks; validation could be bypassed; hand-maintained TypeScript schema and one payload fixture. | Dependency-kind boundary regression tests, exact six-process smoke failures, validated five-family Rust/TypeScript protocol, generated canonical schema, bounded framing, full workspace gate, and a fresh native WebKit screenshot. |
| M02 | Local persistence tests omitted request-bound deduplication, ambiguous commits, atomic artifact finalization, complete projections, migrations, restore, WAL-safe backup, writer exclusion, and orphan recovery. | Thirty-three focused tests cover canonical JSON, atomic history, crash points, artifact corruption/orphans, paged disposable projections, migration/version refusal, hashed online backup, staged restore, and maintenance fencing. |
| M03 | Worker code was mostly value-level scaffolding; the binary was not a real core-gated runtime and recovery was not proven. | Frozen-plan compiler, typed executors, deterministic scheduling/routing, budgets/policies, suspension/no-replay rehydration, model/subagent isolation, and a real framed worker service. |
| M04 | Core/worker snapshot and IPC contracts were incomplete; no real worker crash/restart, canonical port, desktop transaction, or recovery proof. | Typed desktop service, atomic first-input freeze, event-sourced lifecycle, port-neutral commit, real worker supervision/generation fencing, and logical no-effect recovery. |
| M05 | Host authority, adapter lifecycle, redaction, secret leases, and uncertain side-effect settlement lacked complete end-to-end contract tests. | Authenticated frozen admission, bounded process/model/file adapters, universal streaming redaction, scoped revocable leases, and durable proposal-to-settlement ordering. |
| M06 | Portable bytes, filesystem authority, branch publication crash windows, journal-linked recovery, hostile import, and integrity/retention were not completely demonstrated. | Canonical content identities, rooted immutable storage, prepare/publish/verify, noncanonical runtime journal fencing, inert import, child rebinding, repair, and two-scan collection. |
| M07–M08 | Source was still a rough workbench/Chat scaffold and QA had no accepted-geometry, real-browser, native-WebView, high-scale, narrow-layout, or drag/drop evidence. | Integrated workbench and Chat runtime, exact compact geometry, workflow interactions, versioned settings, projection/draft fencing, browser visual gates, and native Tauri/WebKit Broadway screenshot proof. |

## Task-by-task evidence

Path shorthand in the compact tables resolves as follows: `local-store`, `workflow-worker`, `trusted-core`, `capability-host`, and `portable-store` mean their respective `crates/aworkit-*` roots; `workbench`, `shell`, and `chat` mean subdirectories of `desktop/src`. A following bare module name uses the same root as the first path in that row.

### Milestone 01 — foundation and boundaries

| Task | Capability | Implementation evidence | QA evidence and final result |
|---|---|---|---|
| #5 / M01.1 | Rust workspace and process boundaries | `crates/aworkit-process/src/lib.rs`; six process `main.rs` entry points; dependency policy in `qa/check-boundaries.mjs` | J1/group **24 PASS**: Cargo workspace all-target tests; allowed/forbidden dependency fixtures; all six exact handshakes, bounded exits, unknown-argument rejection, and JavaScript-safe generation rejection. |
| #6 / M01.2 | Tauri/React desktop presentation seam | `desktop/src/App.tsx`, `desktop/src/adapters/`, `desktop/src-tauri/src/{main,lib,runtime}.rs`, strict TypeScript/Vite/Tauri configuration | J1/group **24 PASS**: TypeScript, production Vite build, eight Tauri runtime tests, fresh debug native build, process-alive check, and non-placeholder WebKit screenshot. |
| #7 / M01.3 | Shared IDs, envelopes, schema and framing | `crates/aworkit-protocol/src/{lib,history,runtime}.rs`; `protocol/schema/aworkit-envelope.v1.schema.json`; `protocol/generate-runtime-schema.mjs`; five fixtures under `fixtures/protocol/v1/`; `desktop/src/protocol/` | J1/group **24 PASS**: 10 Rust protocol unit tests, two Rust golden tests, six TypeScript protocol tests; invalid IDs/version/kind/safe integers; malformed/oversize/UTF-8/non-JSON and split/coalesced frames; generator hash/freshness. |
| #8 / M01.QA | Complete foundation QA gate | `qa/milestone-01.sh`, `qa/check-boundaries.{sh,mjs}`, `qa/smoke-processes.sh`, `qa/desktop-native-smoke.sh`, boundary fixtures | J1/group **24 PASS**: formatting, workspace all-targets, protocol parity, architecture, process smoke, TypeScript/build, Tauri tests, and native screenshot all ran in one final job. |

### Milestone 02 — canonical local persistence

| Task | Capability | Implementation evidence | QA evidence and final result |
|---|---|---|---|
| #9 / M02.1 | Canonical configuration and workflow JSON | `crates/aworkit-local-store/src/{repository,document_policy,filesystem,manifest}.rs` | J2/group **24 PASS**: exact unknown-field bytes, optimistic conflict, schema rules, forward-schema inert/read-only import/export, secret rejection with `credentialRef` allowance, prewrite size bounds, and symlink rejection. |
| #10 / M02.2 | Atomic local semantic ledger | `crates/aworkit-protocol/src/history.rs`; `local-store/src/{database,ledger}.rs`; `LocalHistoryCommitPort` | J2/group **24 PASS**: event/attempt/checkpoint/outbox/artifact atomicity; backend/Run/head/aggregate fencing; server-derived request hash; dedup conflict; outbox cursors; three injected commit crash points; fresh-connection ambiguity verification/quarantine. |
| #11 / M02.3 | Artifacts and disposable query projections | `local-store/src/{artifacts,projections}.rs` | J2/group **24 PASS**: commit-only artifact finalization, invalid-token rollback, object/range bounds, corruption availability, expired/shared and unindexed crash orphans; Chat/timeline/evidence/search/artifact paging; cursor generations; interrupted rebuild health and retry. |
| #12 / M02.4 | Startup, migration, integrity, backup and restore | `local-store/src/{maintenance,storage,database}.rs` | J2/group **24 PASS**: current/newer/corrupt modes; legacy history/manifest/artifact migrations; future-version refusal; WAL-state online backup; manifest tamper rejection; staged restore with prior-root quarantine; writer exclusion; disposable-projection recovery/exclusion. |
| #13 / M02.QA | Complete canonical-persistence gate | `qa/milestone-02.sh` | J2/group **24 PASS**: **33** local-store tests, formatting, warnings-as-errors, and architecture boundaries. |

### Milestone 03 — deterministic workflow execution

| Task | Capability | Implementation evidence | QA evidence and final result |
|---|---|---|---|
| #14 / M03.1 | Frozen execution plan and typed nodes | `workflow-worker/src/plan.rs`: `ExecutionPlanV1`, canonical snapshot hashes; `node.rs`: sealed `ExecutorRegistryV1`, `NodeTaskV1`, `NodeOutcomeV1` | J3/group **24 PASS**: canonical/tamper-evident snapshots and exact sealed/schema-checked, broker-only executor registry. |
| #15 / M03.2 | Context lineage, branches and joins | `context.rs`: `ContextStore`, `ContextRevision`, `JoinContract`, `ChildIntegration`; `branch.rs`: branch frames/checkpoints/coordinator | J3/group **24 PASS**: independent revisions, declared-order joins, conflict rejection, and once-only child integration. |
| #16 / M03.3 | Deterministic scheduler and frozen routing | `scheduler.rs`: `SchedulerV1` and admission proposals; `routing.rs`; `gateway.rs`: `CoreGatewayV1` | J3/group **24 PASS**: deterministic progress, commit-ack gating, loop-capacity reservation, non-truthy predicates, stable IDs, retransmit fencing, and control lane. |
| #17 / M03.4 | Hierarchical limits and attempt policy | `limits.rs`: `LimitLedger`, `BudgetEnvelope`; `policy.rs`: `AttemptLedger`, `AttemptPolicyV1` | J3/group **24 PASS**: ancestor charging once; retry/reconcile/evaluator/gate/fallback/exhaustion; uncertain effects never receive a new invocation ID. |
| #18 / M03.5 | Suspension and no-replay rehydration | `suspension.rs`: `SuspensionControllerV1`, checkpoints, `RehydratorV1`; `runtime.rs`: `WorkerServiceV1` | J3/group **24 PASS**: exact input/approval/pause/cancel state, dedup/checkpointing, generation/hash/cursor fences, and reconciled outcomes without effect replay. |
| #19 / M03.6 | Model-agent loop and temporary subagents | `agent.rs`: `AgentLoopV1`, `SubagentManagerV1`; child integration in `context.rs` | J3/group **24 PASS**: turn settlement, tool bounds, child depth/context/budget isolation, and declared parent-revision integration. |
| #20 / M03.QA | Complete worker gate | `qa/milestone-03.sh`; `workflow-worker/tests/{milestone_03,milestone_03_runtime}.rs` | J3/group **24 PASS**: **20** tests (6 foundation + 14 runtime), warnings-as-errors, implementation-dependency rejection, and shared boundary gate. |

### Milestone 04 — headless local Chat runtime

| Task | Capability | Implementation evidence | QA evidence and final result |
|---|---|---|---|
| #21 / M04.1 | Project coordination and typed desktop API | `trusted-core/src/project.rs`: `ProjectCoordinator`; `desktop.rs`: `DesktopApi`, `DesktopTransactionV1`, `serve_core_stdio` | J4/group **24 PASS**: document/identity/secret rules, atomic idempotent cursor-bounded transactions, and a real framed trusted-core service. |
| #22 / M04.2 | Authority and atomic first-input freeze | `authority.rs`: `SnapshotFreezerV1`, authority manifest, approvals and graph hash; `lifecycle.rs` start transitions | J4/group **24 PASS**: rejected starts remain editable; successful starts freeze atomically; authority drift/unresolved inputs fail; grants expire/are single-use; join order affects identity. |
| #23 / M04.3 | Event-sourced Chat/Run aggregate | `lifecycle.rs`: `RunAggregateV1`, commands/events/states and exact fold | J4/group **24 PASS**: queue, attempt, wait, pause, cancel, retry, fork, continue, terminal legality, idempotency, and generation-specific recovery states. |
| #24 / M04.4 | Canonical local commit and outbox | `committer.rs`: `CanonicalCommitter`, binding/request/outcome DTOs; committed desktop transaction boundary | J4/group **24 PASS**: process-neutral commit port, dedup/conflict/checkpoint/outbox behavior, and desktop atomicity. |
| #25 / M04.5 | Real worker supervision | `supervisor.rs`: `ProcessWorkerSupervisorV1`; worker `gateway.rs` and `runtime.rs` framed service | J4/group **24 PASS**: real child handshake, generation-1 crash, generation-2 restore, stale fencing, heartbeats, controls, framed shutdown, and bounded cleanup. |
| #26 / M04.6 | Exact logical recovery | `recovery.rs`: `LocalRecovery`, recovery facts/decisions; lifecycle rehydration target; worker `RehydratorV1` | J4/group **24 PASS** with J3/group 24 worker evidence: fenced logical envelope, uncertainty preservation, new generation, and no replay of effects. |
| #27 / M04.QA | Complete headless-core gate | `qa/milestone-04.sh`; `trusted-core/tests/milestone_04.rs` | J4/group **24 PASS**: **16** tests (3 unit + 13 integration), real worker crash/recovery, warnings-as-errors, implementation-dependency rejection, and boundary gate. |

### Milestone 05 — secure capability execution

| Task | Capability | Implementation evidence | QA evidence and final result |
|---|---|---|---|
| #28 / M05.1 | Authenticated generation-frozen gateway | `capability-host/src/registry.rs`: frozen descriptors; `gateway.rs`: signed `ApprovedInvocationEnvelopeV1`, `CapabilityHost::admit_v1` | J5/group **24 PASS**: authentication/tamper, generation/hash/scope/lease/deadline drift, canonical authority, backpressure, cancellation, and active/completed dedup. |
| #29 / M05.2 | Safe process, shell and Python adapters | `capability-host/src/{process,tools}.rs`: native/hermetic ports, controlled runner, exact authority modes | J5/group **24 PASS**: hermetic facts, authority/sandbox non-downgrade, prelaunch cancellation, real timeout/process-tree cleanup, output bounds, and cleared environment. |
| #30 / M05.3 | Frozen model/provider gateway | `capability-host/src/model.rs`: `FrozenModelGateway`, frozen resolution, provider engine/events | J5/group **24 PASS**: exact provider binding/version, ordered fallback, ambiguity stop, stream/input/output limits, exactly-one usage, cancellation, duplicate rejection. |
| #31 / M05.4 | Rooted file read/search/edit | `capability-host/src/files.rs`: `ProjectFiles::{read_v1,search_v1,edit_v1}` and effect descriptors | J5/group **24 PASS**: traversal/symlink/root replacement, bounds, cancellation, write denial, optimistic-hash conflict, atomic edit and truthful effect facts. |
| #32 / M05.5 | Normalized redacted outcomes | `capability-host/src/{normalize,materialize}.rs`: shared streaming `Redactor`, `InvocationNormalizer`, conservative classification | J5/group **24 PASS**: split-secret redaction across every content class, monotonic events, one terminal, and retry-safety truth table. |
| #33 / M05.6 | Scoped secret broker and leases | `trusted-core/src/secrets.rs`: `SecretBroker`; host `materialize.rs`: `SecretMaterializer` | J5/group **24 PASS**: audience/decision/invocation/field/use/TTL fencing, replacement/run/generation revocation, audit, exact field injection, and shared redactor. |
| #34 / M05.7 | Durable invocation broker | `trusted-core/src/broker.rs`: `DurableInvocationBroker` and durable attempted/authorized/progress/settled outboxes | J5/group **24 PASS**: proposal→approval→lease→dispatch→settlement→worker delivery order, idempotency, rollback, crash ambiguity committed as uncertain, and no replay. |
| #35 / M05.QA | Complete capability-security gate | `qa/milestone-05.sh`; host/core M05 integration suites | J5/group **24 PASS**: six host + core suites covering gateway, adapters, redaction, leases and durable broker; formatting, boundaries, all targets, and warnings-as-errors. |

### Milestone 06 — portable history and recovery

| Task | Capability | Implementation evidence | QA evidence and final result |
|---|---|---|---|
| #36 / M06.1 | Canonical portable bytes and deny-before-hash export | `portable-store/src/{codec,export,artifact}.rs`: canonical codec, segments/checkpoints/snapshot hash, export policy | J6/group **24 PASS**: RFC8785/UTF-16 ordering, LF framing, domain hashes, ordinal/parent/tamper checks, bounded checkpoints, recursive forbidden fields, and explicit omissions. |
| #37 / M06.2 | Rooted filesystem and read-only Git facts | `portable-store/src/{workspace,repository}.rs`: `WorkspaceRoot`, `ProjectReference`, immutable publication, Git inspection | J6/group **24 PASS**: traversal/symlink/case alias/root replacement; Git HEAD/ref facts; byte proof that `.git` was not mutated. |
| #38 / M06.3 | Format negotiation and immutable manifests | `portable-store/src/manifest.rs`: repository/session/branch catalogs and compatibility; strict codec validation | J6/group **24 PASS**: read-write/newer-minor read-only/unsupported negotiation, immutable round trips, malformed context and self-parent rejection. |
| #39 / M06.4 | CAS artifacts and prepare/publish/verify commit | `portable-store/src/{artifact,commit,port,repository}.rs`; `PortableCanonicalCommitPort` | J6/group **24 PASS**: no head during prepare, exact retry/change/head conflict, after-prepare/before-head/after-head faults, reread verification, hash mismatch, ranges and inert media. |
| #40 / M06.5 | Canonical router and noncanonical runtime journal | Protocol portable DTOs; `trusted-core/src/{committer,portable}.rs`; `local-store/src/portable_journal.rs` | J6/group **24 PASS**: pending-before-publish, exact generation 0→1/1→2, restart durability, stale/missing journal, every publication/finalization crash point, backend freeze, and no semantic replay. |
| #41 / M06.6 | Hostile import quarantine and read-only projections | `portable-store/src/projection.rs`: import/lineage/page validation and verified evidence | J6/group **24 PASS**: whole-lineage acceptance, command-shaped data remains inert, omissions/artifact metadata, bounded paging/source-token invalidation, and corrupt-segment rejection. |
| #42 / M06.7 | Fresh child continuation, repair and retention | `portable-store/src/{rebind,integrity,manifest}.rs`: child plans, integrity engine, non-destructive repair, verified orphan collection | J6/group **24 PASS**: missing/incompatible/ambiguous bindings, fresh child IDs/snapshot/authority, no imported approval/secret/runtime, typed corruption, exact repair proposals, two scans + grace + locked head. |
| #43 / M06.QA | Complete portable-history gate | `qa/milestone-06.sh`; portable/core M06 suites | J6/group **24 PASS**: portable tests plus protocol golden, 33 local-store, and core recovery suites; formatting, boundaries, all targets and warnings-as-errors. |

### Milestone 07 — desktop workbench and workflow editor

| Task | Capability | Implementation evidence | QA evidence and final result |
|---|---|---|---|
| #44 / M07.1 | Appearance, density and accessibility tokens | `desktop/src/workbench/appearance.ts`, `desktop/src/styles.css`: System/Light/Dark resolution, semantic tokens, pre-render application, contrast helpers | J7/group **24 PASS**: token parity/contrast, exact compact dimensions, appearance-ready first render, dark token `#111318`, focus/forced-color/reduced-motion contracts, 200% and narrow-window browser checks. |
| #45 / M07.2 | Typed command/event projection gateway | `workbench/projection.ts`: strict Zod receipts/events and `ProjectionGateway`; `workbench/corePort.ts` | J7/group **24 PASS**: immutable ordered projection, sequence-gap stale freeze, explicit newer snapshot resync, stable command IDs, malformed receipts, and native-port version fencing. |
| #46 / M07.3 | Persistent compact workbench shell | `desktop/src/App.tsx`; `shell/{NavigationPane,PaneSplitter,ManagementScreen}.tsx`; adapter facades | J7/group **24 PASS**: accepted navigation/pane/header/inspector geometry, keyboard splitter and focus, route/draft persistence, no horizontal overflow, browser screenshots, and native WebKit proof. |
| #47 / M07.4 | Versioned settings and capability resolution | `workbench/{settings,SettingsScreen,corePort}.ts(x)`; Tauri runtime settings snapshot/commit | J7/group **24 PASS**: requirement resolution, authority preview, stale version rejection, stable idempotent commit, complete draft retention across routes, and accessible controls. |
| #48 / M07.5 | Lossless workflow editor kernel | `workbench/workflow.ts`: parse/serialize/edit/undo/redo/connect/delete/property validation | J7/group **24 PASS**: unknown-field preservation, typed selection, edit coalescing, undo/redo, schema-driven drafts, cycles/self-loops/multi-edges, and representative 1,000-node frame-budget gate. |
| #49 / M07.6 | Replaceable graph surface | `workbench/graphSurface.tsx`: `WorkflowGraphSurfacePort`/adapter; `WorkflowEditorScreen.tsx` | J7/group **24 PASS**: lossless ports/groups/edges, six-node accepted graph, dependency-gated Run, selection callbacks, and real-browser drag/drop adding the seventh node. |
| #50 / M07.QA | Complete workbench/visual gate | `qa/milestone-07.sh`, `qa/desktop-browser-visual.sh`, `qa/desktop-native-smoke.sh` | J7/group **24 PASS**: **18** Vitest tests in four files, eight Tauri tests, TypeScript, Vite, 1440×940 browser geometry/screenshots, drag/drop, dark/200%/narrow modes, and native Tauri/WebKit screenshot. |

### Milestone 08 — desktop Chat and evidence experience

| Task | Capability | Implementation evidence | QA evidence and final result |
|---|---|---|---|
| #51 / M08.1 | Live Chat workspace projection | `chat/{workspace,corePort,useChatRuntime,ChatWorkspaceScreen}.ts(x)` and Tauri `DesktopRuntime` | J8/group **24 PASS**: contiguous projection, stale freeze/resync after gaps, stable runtime events, one-Chat/one-Run header, Run controls, selected evidence, route handoff, and accepted Chat geometry. |
| #52 / M08.2 | Composer and Run controls | `chat/{composer,ChatComposer}.ts(x)` | J8/group **24 PASS**: first-send vs queued-input intents, IME block, attachment/workflow inputs, terminal legality, projection-derived controls, stable retry ID, and draft retained until confirmed receipt. |
| #53 / M08.3 | Semantic conversation cards | `chat/{conversation,ConversationTimeline}.ts(x)` | J8/group **24 PASS**: known item transforms, source-provided reasoning labels, escaped content, typed action target/intent, bounded visible window, and future/unknown records retained as inspectable raw data. |
| #54 / M08.4 | Evidence query and safe inspector | `chat/{evidence,EvidenceInspector}.ts(x)` | J8/group **24 PASS**: category paging/filtering, exact safe JSON, available/redacted/expired/unsupported/opaque states, secret-value suppression, and no inferred unavailable details. |
| #55 / M08.QA | Complete Chat/native visual gate | `qa/milestone-08.sh`, Chat kernel/runtime/app integration suites, browser/native visual scripts | J8/group **24 PASS**: trusted-core suite, eight Tauri tests, **20** Vitest cases in three files, TypeScript, Vite, accepted browser geometry/interactions and native Tauri/WebKit screenshot. |

## Final QA job ledger

All eight job executions below belong to consolidated Adashi QA run group **#24**, which completed with **8 passed, 0 failed, 0 timed out**. This supersedes the earlier per-job runs as the primary completion evidence.

| Job | Tasks | Runner | Consolidated run | Result and principal categories |
|---|---|---|---|---|
| J1 — Milestone 01 boundary contracts | #5–#8 | `qa/milestone-01.sh` | **24** | **PASSED** — full workspace all-targets, schema/golden/framing, dependency boundaries, six-process smoke, TypeScript/Vite, Tauri tests and Broadway native screenshot. |
| J2 — Milestone 02 canonical local persistence | #9–#13 | `qa/milestone-02.sh` | **24** | **PASSED** — 33 canonical repository, transaction, crash, artifact, projection, migration, corruption, backup/restore and writer-gate tests; warnings/boundaries. |
| J3 — Milestone 03 deterministic workflow execution | #14–#20 | `qa/milestone-03.sh` | **24** | **PASSED** — 20 plan/context/scheduler/routing/limits/policy/suspension/rehydration/agent/runtime tests; warnings and process boundaries. |
| J4 — Milestone 04 headless local Chat runtime | #21–#27 | `qa/milestone-04.sh` | **24** | **PASSED** — 16 core tests including real framed core/worker, crash/new-generation restore, lifecycle, authority, canonical commit and no-replay recovery. |
| J5 — Milestone 05 secure capability execution | #28–#35 | `qa/milestone-05.sh` | **24** | **PASSED** — frozen admission, process/model/file adapters, redaction/outcome truth, scoped secrets and durable broker; all targets/warnings/boundaries. |
| J6 — Milestone 06 portable history and recovery | #36–#43 | `qa/milestone-06.sh` | **24** | **PASSED** — canonical bytes, rooted filesystem, manifests, publication crash windows, journal recovery, import, rebind/integrity/retention plus protocol/local/core regressions. |
| J7 — Milestone 07 desktop workbench and workflow editor | #44–#50 | `qa/milestone-07.sh` | **24** | **PASSED** — 18 Vitest + 8 Tauri tests, type/build, browser geometry/interaction modes, screenshots and native WebKit screenshot. |
| J8 — Milestone 08 desktop Chat and evidence experience | #51–#55 | `qa/milestone-08.sh` | **24** | **PASSED** — core + 8 Tauri + 20 desktop tests, type/build, Chat/workflow browser visuals/interactions and native WebKit screenshot. |

## Native and browser visual proof

The consolidated run group #24 executions of J1, J7 and J8 rebuilt `desktop/src-tauri/target/debug/aworkit-desktop`, rejected a binary older than source or bundled assets, launched the actual Tauri/WebKit process, and rendered it through a clean headless GTK Broadway display. The recorded screenshot was **1600×1014**, contained **1,831** sampled colors, and had a **37.8%** dominant-color fraction (`backend=broadway`). The native process remained alive and the gate explicitly rejected a blank or Milestone 01-like single-color placeholder. Both visual runners now fail closed if Firefox, WebDriver, Broadway, Pillow, or the supporting command-line tools are absent; they cannot turn a skipped render into a green milestone.

The J7 and J8 real-Firefox production-bundle gate separately proved:

- 1440×940 compact Chat geometry: 208px navigation, 48px header, approximately 320px evidence inspector, bounded composer, appearance initialized, no horizontal overflow, and no `Milestone 01` placeholder text;
- keyboard resizing from 208px to 216px and back with focus retained;
- the accepted six-node workflow, unresolved dependency/disabled-Run state, properties selection, and actual drag/drop increasing the graph to seven nodes;
- Dark mode's `--aw-window` value of `#111318`;
- a 760px narrow window collapsing navigation to 44px without horizontal overflow;
- a true 2× device scale at a 720×470 CSS viewport, again with the 44px compact navigation and no overflow;
- non-blank Chat and workflow screenshots (`2,162`/`2,171` sampled colors; `29.6%`/`33.1%` dominant fractions).

These two gates are complementary: Firefox verifies exact DOM geometry, state and interaction contracts, while Broadway captures the real native Tauri/WebKit surface.

## Scope boundary

This audit claims only Adashi tasks #5–#55, corresponding to Milestones 01–08. **Milestone 09 and every later milestone remain future work and are not claimed as implemented, tested, or complete by this report.** Likewise, passing these milestone contracts does not imply completion of later roadmap capabilities such as further management/repair experiences, distribution and platform activation profiles, or any other M09+ feature.
