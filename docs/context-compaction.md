# Context compaction

Implementation specification and reference audit, 2026-09-08.

Reference: `C:/src/deepseek-harness`, commit `b150a551b8d465e31e418e1b2eaf5e79bbb7d28e`. The implementation is Rust-owned under Aworkit's frozen provider and tool authority. Compaction changes the model-visible selection; original semantic events, tool outcomes, instruction metadata and user messages remain immutable.

## Reference behavior

| Concern | DeepSeek implementation |
| --- | --- |
| Timing | `compaction-basic/src/index.ts`: serial pre-step pressure check before deriving the next request. No first-request automatic pressure check until a durable route exists. |
| Pressure | `token-meter/src/index.ts`: provider input + cache read/write + output anchor, plus heuristic changes since the response. Anchor accepted only for an equal canonical envelope and when usage is at least its heuristic price. Changed envelope falls back to full estimation. |
| Estimation | `token-meter/src/estimate.ts`: four UTF-16 code units per token, four per block and role; recursive tool-result content; system and tool schemas counted. No tokenizer or arbitrary output-reserve subtraction. |
| Policy | 80% of exact routed model context capacity; retain 16%, or configured absolute tokens. Exact provider/model overrides; unknown capacity produces an actionable diagnostic. Default summary cap 8192, one additional pressure reduction, one consecutive overflow recovery. |
| Pruning | Optional `compaction-tool-result-pruner`: qualifying pressure or overflow first replaces text middles over 8192 Unicode code points with head 4096 + exact marker + tail 1024. Rich blocks stay ordered, errors and call IDs preserved. Remeasure after durable pruning. |
| Selection | `compaction-basic/src/region.ts`: walk backwards to the retained budget, always keep at least the last surface node, move the cut backwards to a balanced tool boundary. A closed early step in the same user turn can compact. Explicit ranges use surface positions, never numeric event ordering. |
| Summary | `summarizer.ts`: same system/tools and selected leading messages, final user-role directive with eight required sections. One isolated model call, no agent/tool loop. Configured target, latest route, then agent target precedence. Original auxiliary output and usage retained separately. |
| Admission | Reject empty, visual, failed, cancelled or token-truncated output. Keep text only. Frame in a user checkpoint with `<compacted-summary>`; reject unless the entire framed replacement is strictly smaller. Existing summaries must be consolidated, not copied. |
| Transaction | Durable start before calling model, source snapshot/stability check, summary plus replacement, exactly one attempted end on success/failure. Original events are shadowed, not deleted. Cancellation has precedence. |
| Overflow | Only canonical context-window-exceeded errors qualify. Prune then force a balanced reduction retaining the last unit even below threshold. Retry only after replacement generation advances; a successful prune still permits retry if later summarization fails. Successful assistant response resets the consecutive retry counter. |
| Manual | `compactNow` runs idle maintenance, no-op without writes when no useful range. Queue new prompts FIFO until maintenance settles. Selected-span stability permits unrelated appended context; flush completed/failed lifecycle before admitting queued work. `/compact` is not a model tool. |
| Integration | Session surface replay, persistence/checkpoints, token accounting and pressure UI, request-error recovery, workspace instruction re-injection, Skills catalog, repeat-tool notices, subagent isolation and command correlation all consume the same replacement selection. |

## Aworkit implementation contract

1. Preserve the algorithm, defaults, exact summary directive and checkpoint framing. Freeze compaction configuration and model capacity with the Chat. Use the existing approved model gateway for isolated auxiliary calls.
2. Keep a durable context document per Chat/node/branch/child, with original conversation and exchange cursors. Import each settled exchange once. Never replay historical tools or grow the active loop checkpoint with already-shadowed exchanges.
3. Use one estimator for selection, strict shrink validation, pressure anchors and inspection. Keep cumulative billing separate from context occupancy; include auxiliary usage in Run totals.
4. Commit original source identities/hashes, trigger, selected positional units, pruning reductions, replacement document and auxiliary provenance. A failed summary cannot replace the current context. A failed durability commit cannot admit a request.
5. Apply replacements before Workspace Instructions reconciliation. Validate actual surviving typed instruction references, restore missing baseline and active nested scopes, preserve removals, handle unavailable sources with existing stale fallback. Changing context generation re-arms restoration even with warm caches. Summary prose never proves visibility.
6. Cover tool-calling and text-only Agents, model-call nodes, approval continuation, timeout and canonical overflow retries, subsequent Chat inputs, context edits and reopen. Child contexts remain independent and use their selected frozen tools only.
7. Provide manual compaction at the context control, with idle admission, cancellation, visible lifecycle/errors and durable completion. Keep original timeline/evidence available and expose the replacement through Display Context.
8. Validate real provider payloads and native WebView behavior, including repeated compaction and the mandatory workspace-instruction integration gate in `design-workspace-instructions.md`.

## Implementation and ownership

The production implementation lives in `desktop/src-tauri/src/runtime/compaction`. `surface.rs` provides UTF-16 pricing, positional selection, indivisible closed exchanges and Unicode-safe rich-result pruning. `runtime.rs` owns restoration, pressure decisions, isolated summarization, commit fencing and checkpoint admission. `provider.rs` resolves the frozen auxiliary target through an invocation-scoped credential lease; the acting model cannot choose or call it. `target.rs` validates and freezes the exact Settings selection, including text/tools/vision compatibility and opaque credential revision.

`model_tool_loop.rs` and `graph_pass.rs` call this boundary for tool Agents, text-only Agents, explicit model-call nodes and child contexts. Approval continuation imports only authority-settled exchanges beyond its absolute cursor. Timeout notices and Skill contributions remain in the selected surface. Canonical overflow classification is shared by OpenAI-compatible, Anthropic and Gemini adapters; quota, authentication, network and output-limit errors do not authorize compaction retries.

The semantic stream stores `context.checkpoint`, `context.usage`, `context.compaction-started`, `context.compacted`, `context.compaction-ended` and diagnostic events. A source records its owner, generation, original semantic event IDs, ordered unit hashes, typed instruction references and settled-tool cursor. Summary replacement and checkpoint commit in one transaction. A crash before settlement is recovered from that committed transaction; an unfinished start gets exactly one terminal recovery event. Earlier requests and original tool results remain available as immutable evidence.

Forking freezes the parent's selected document, its source sequence, and an explicit owner/ID allowlist of instruction records. Child preparation validates that allowlist against original core records, then reconciles the child's independent selection. Later parent updates cannot enter the fork. This preserves active nested scopes and stale-source fallback without treating summary prose as instruction provenance.

Context details expose **Compact context** while the Chat is idle. A version fence rejects stale controls. Maintenance creates no user or assistant message, leaves the Chat usable after a summary failure, and records billed auxiliary usage separately from occupancy. Inputs submitted during maintenance wait FIFO in the desktop session queue; each uses the settled predecessor's current history fence. Uncertain admission retains the original command ID and input. This queue has the lifetime of the open desktop session; committed commands and context survive restart.

## Aworkit-specific design choices

- Defaults are 80% pressure, 16% retained tail, 8192 summary output tokens, one additional pressure reduction and one overflow recovery. Pruning is enabled in the integrated default policy and can be disabled independently. All controls are available per exact model in Settings; there is no competing route-pattern allowlist.
- Aworkit freezes the acting route and capacity at Chat creation. It can measure before the first request; Harness waits for its first durable dynamic route. An explicit frozen summary target overrides the acting model. Aworkit has no separate dynamic route or agent fallback to guess at execution time.
- Complete parallel tool exchanges are atomic surface units. Keeping or removing that unit preserves calls, results, error flags, ordering and opaque provider continuation metadata together. This implements balanced boundaries using Aworkit's existing exchange representation.
- The existing core store and node transport limits remain enforced. Byte pressure can trigger reduction earlier than the model token threshold. Auxiliary requests have their own bounded 768 KiB source and 128 KiB result allowance, so the acting node's smaller output allowance does not truncate summaries. An irreducible final unit or unavailable/failed summary never authorizes deleting evidence or an unchanged overflow retry.
- New Chats freeze compaction version, policy, capacity and summary binding. Pre-upgrade Chats preserve their original frozen authority serialization and invocation identities: default byte-pressure, canonical overflow and manual maintenance remain available; exact token-pressure configuration and optional summary routing are adopted in a new Chat. Settings changes never silently alter an existing Chat's model or credentials.
- Source fencing is intentionally stricter than Harness's selected-span append tolerance: a conflicting context revision rejects the summary. The desktop core serializes maintenance and queues input, so an unrelated user append cannot race a valid maintenance commit.
- Full bounded context documents are immutable revisions in the existing semantic store, with source identities and exchange cursors. This does not introduce another conversation archive, replay tools, or mutate prior requests. The context panel remains the existing editor; no separate `/compact` parser or model-visible compaction tool is introduced.

## Verification

Validated on Windows using the actual Tauri/WebView2 application and a loopback OpenAI-compatible provider, with separate frozen acting and summary model IDs. The isolated profiles and provider-payload captures are reproducible with `desktop/scripts/native-compaction-smoke.mjs` and `AWORKIT_QA_BINARY`. `AWORKIT_QA_QUEUE=1` holds the first auxiliary request while a real composer input queues. `AWORKIT_QA_TEXT_ONLY=1` exercises the separate text-only Agent execution path.

| Check | Result |
| --- | --- |
| Desktop Rust suite | 275 passed |
| Capability host suite | 91 passed, 2 existing ignored tests |
| Frontend suite | 297 passed across 39 files; serial execution with 30-second timeout for the existing slow Settings navigation test |
| Frontend production build / native build | Passed; existing bundle-size and mixed-import warnings only |
| Native tool Agent | Manual button, separate summary model, exact reduced provider payload, original evidence, restored instructions, canonical HTTP overflow plus one retry, restart, fork and balanced lifecycle passed |
| Native queued input | Same flow with input held until manual settlement passed; a two-input integration test verifies FIFO order and changing history fences |
| Native text-only Agent and explicit model-call node | Manual compaction, typed overflow recovery, restart and fork passed |
| Instruction integration gate | Actual replacement tests cover warm caches, changed/deleted/unavailable sources, active nested scopes, removal tombstones, partial survival, successive reductions, restoration budget, cancellation, reopen and Chat/node/branch/child isolation |
| Summary admission failures | Empty, failed, cancelled, nonshrinking and concurrent-edit cases preserve the prior selection; auxiliary usage and lifecycle remain separate |
| Automatic pressure and auxiliary isolation | A still-large accepted summary triggers the permitted additional reduction; pressure stops below threshold. Auxiliary tool requests remain raw evidence and never dispatch |

Native evidence directories under `desktop/src-tauri/target`: `native-compaction-1788869753292` (tool Agent), `native-compaction-1788869759124` (queue), `native-compaction-1788870054704` (text-only), and `native-compaction-1788870326025` (explicit model-call node, reproduced with `AWORKIT_QA_MODEL_CALL=1`). Each contains `report.json`, captured provider requests, native logs and a context-panel screenshot. These fixtures prove transport, authority, persistence and UI behavior; they do not claim a quality benchmark of a live model's summaries.

The formal interaction contract is `uml.workflow_worker.context_compaction`, attached to `aworkit.workflow_worker.context_store` in Adashi (revision 563).
