# Context compaction

Implementation specification and reference audit, 2026-09-08.

Reference: `C:/src/deepseek-harness`, commit `b150a551b8d465e31e418e1b2eaf5e79bbb7d28e`. The implementation is Rust-owned under Aworkit's frozen provider and tool authority. Compaction changes the model-visible selection; original semantic events, tool outcomes, instruction metadata and user messages remain immutable.

## Reference behavior

| Concern | DeepSeek implementation |
| --- | --- |
| Timing | `compaction-basic/src/index.ts`: serial pre-step pressure check before deriving the next request. No first-request automatic pressure check until a durable route exists. |
| Pressure | `token-meter/src/index.ts`: provider input + cache read/write + output anchor, plus heuristic changes since the response. Anchor accepted only for an equal canonical envelope and when usage is at least its heuristic price. Changed envelope falls back to full estimation. |
| Estimation | `token-meter/src/estimate.ts`: four UTF-16 code units per token, four per block and role; recursive tool-result content; system and tool schemas counted. No tokenizer or arbitrary output-reserve subtraction. |
| Policy | 80% of exact routed model context capacity; retain 16%, or configured absolute tokens. Exact provider/model overrides; unknown capacity produces an actionable diagnostic. Summary and carried-user-turn budgets are derived from the retention ratio, one additional pressure reduction, one consecutive overflow recovery. |
| Pruning | Optional `compaction-tool-result-pruner`: qualifying pressure or overflow first replaces text middles over 8192 Unicode code points with head 4096 + exact marker + tail 1024. Rich blocks stay ordered, errors and call IDs preserved. Remeasure after durable pruning. |
| Selection | `compaction-basic/src/region.ts`: walk backwards to the retained budget, always keep at least the last surface node, move the cut backwards to a balanced tool boundary. A closed early step in the same user turn can compact. Explicit ranges use surface positions, never numeric event ordering. |
| Summary | `summarizer.ts`: same system/tools and selected leading messages, final user-role directive with eight required sections. One isolated model call, no agent/tool loop. Configured target, latest route, then agent target precedence. Original auxiliary output and usage retained separately. |
| Admission | Reject empty, visual, failed, cancelled or token-truncated output. Keep text only. Frame in a user checkpoint with `<compacted-summary>`; reject unless the entire framed replacement is strictly smaller. Existing summaries must be consolidated, not copied. |
| Transaction | Durable start before calling model, source snapshot/stability check, summary plus replacement, exactly one attempted end on success/failure. Original events are shadowed, not deleted. Cancellation has precedence. |
| Overflow | Only canonical context-window-exceeded errors qualify. Prune then force a balanced reduction retaining the last unit even below threshold. Retry only after replacement generation advances; a successful prune still permits retry if later summarization fails. Successful assistant response resets the consecutive retry counter. |
| Frozen input bound | An exact provider request that exceeds the frozen plan's `maximumInputBytes` is a context condition, not a malformed plan. The gateway reports it as `InputBoundExceeded` with both sizes, and the Agent loop and text-only/model-call turn recover it through the same bounded balanced reduction used for canonical overflow. A turn whose bound rejection cannot be reduced reports the bound instead of a plan defect, so the user sees an actionable size instead of `frozen provider plan is invalid`. |
| Manual | `compactNow` runs idle maintenance, no-op without writes when no useful range. Queue new prompts FIFO until maintenance settles. Selected-span stability permits unrelated appended context; flush completed/failed lifecycle before admitting queued work. `/compact` is not a model tool. |
| Integration | Session surface replay, persistence/checkpoints, token accounting and pressure UI, request-error recovery, workspace instruction re-injection, Skills catalog, repeat-tool notices, subagent isolation and command correlation all consume the same replacement selection. |

## Aworkit implementation contract

1. Preserve the algorithm, defaults, exact summary directive and checkpoint framing. Freeze compaction configuration and model capacity with the Chat. Use the existing approved model gateway for isolated auxiliary calls.
2. Keep a durable context document per Chat/node/branch/child, with original conversation and exchange cursors. Import each settled exchange once. Never replay historical tools or grow the active loop checkpoint with already-shadowed exchanges.
3. Use one estimator for selection, strict shrink validation, pressure anchors and inspection. Keep cumulative billing separate from context occupancy; include auxiliary usage in Run totals. Cumulative token and cost totals are reporting only: main Agents, subagents, text-only nodes and approval continuations have no aggregate run budget. Legacy frozen budget fields do not terminate execution or reject outcome persistence.
4. Commit original source identities/hashes, trigger, selected positional units, pruning reductions, replacement document and auxiliary provenance. A failed summary cannot replace the current context. A failed durability commit cannot admit a request.
5. Apply replacements before Workspace Instructions reconciliation. Validate actual surviving typed instruction references, restore missing baseline and active nested scopes, preserve removals, handle unavailable sources with existing stale fallback. Changing context generation re-arms restoration even with warm caches. Summary prose never proves visibility.
6. Cover tool-calling and text-only Agents, model-call nodes, approval continuation, timeout and canonical overflow retries, subsequent Chat inputs, context edits and reopen. Child contexts remain independent and use their selected frozen tools only.
7. Provide manual compaction at the context control, with idle admission, cancellation, visible lifecycle/errors and durable completion. Keep original timeline/evidence available and expose the replacement through Display Context.
8. Validate real provider payloads and native WebView behavior, including repeated compaction and the mandatory workspace-instruction integration gate in `design-workspace-instructions.md`.

## Implementation and ownership

The production implementation lives in `desktop/src-tauri/src/runtime/compaction`. `surface.rs` provides UTF-16 pricing, positional selection, indivisible closed exchanges and Unicode-safe rich-result pruning. `runtime.rs` owns restoration, pressure decisions, isolated summarization, commit fencing and checkpoint admission. `provider.rs` resolves the frozen auxiliary target through an invocation-scoped credential lease; the acting model cannot choose or call it. `target.rs` validates and freezes the exact Settings selection, including text/tools/vision compatibility and opaque credential revision.

`model_tool_loop.rs` and `graph_pass.rs` call this boundary for tool Agents, text-only Agents, explicit model-call nodes and child contexts. Approval continuation imports only authority-settled exchanges beyond its absolute cursor. Timeout notices and Skill contributions remain in the selected surface. Canonical overflow classification is shared by OpenAI-compatible, Anthropic and Gemini adapters; quota, authentication, network and output-limit errors do not authorize compaction retries.

The semantic stream stores `context.checkpoint`, `context.usage`, `context.compaction-started`, `context.compacted`, `context.compaction-ended` and diagnostic events. A source records its owner, generation, original semantic event IDs, ordered unit hashes, typed instruction references and settled-tool cursor. Summary replacement and checkpoint commit in one transaction. A crash before settlement is recovered from that committed transaction; an unfinished start gets exactly one terminal recovery event. Earlier requests and original tool results remain available as immutable evidence.

Preparation runs before every provider request and emits no events while it works, so a slow turn used to be unattributable. It now ends with one `context.prepare-timing` diagnostic naming each phase (`lifecycle`, `compression-scope`, `restore`, `instructions`, `pressure`, `manage`, `checkpoint`) in milliseconds plus the total, which makes the cost of a stalled preparation measurable from the stored stream rather than inferred.

### Auxiliary prompt prefix reuse

The auxiliary summary request is the selected leading surface plus the summary directive, so its prompt is a strict prefix extension of the conversation the acting turn just sent: everything it shadows was already in the previous request, and only the directive is new. That property has to hold on the wire as well as in the message list. `serde_json` writes object keys in name order, so a request-scoped parameter such as the auxiliary `max_tokens` cap would be written before `messages`, making the leading bytes of the body differ and limiting the reusable prefix to the three bytes `{"m`. The recorded test1_qwen run showed exactly that: `context.compacted.auxiliary` reported `commonPrefixBytes` 3 on a 540,125 byte summary prompt while the preceding turn shared 938,080 of its 958,677 bytes.

The OpenAI-compatible transport now writes each request body once, with `messages` first, and sends those exact bytes, so the measured body is the sent body and a parameter can never sit in front of the prompt. The summary call keeps its output cap; it no longer reshapes the request. This is measured locally by `sentBytes`/`commonPrefixBytes`; whether a provider bills the shared prefix as cached remains its own `prompt_cache_hit_tokens` report.

Forking freezes the parent's selected document, its source sequence, and an explicit owner/ID allowlist of instruction records. Child preparation validates that allowlist against original core records, then reconciles the child's independent selection. Later parent updates cannot enter the fork. This preserves active nested scopes and stale-source fallback without treating summary prose as instruction provenance.

Context details expose **Compact context** while the Chat is idle. A version fence rejects stale controls. Maintenance creates no user or assistant message, leaves the Chat usable after a summary failure, and records billed auxiliary usage separately from occupancy. Inputs submitted during maintenance wait FIFO in the desktop session queue; each uses the settled predecessor's current history fence. Uncertain admission retains the original command ID and input. This queue has the lifetime of the open desktop session; committed commands and context survive restart.

## Aworkit-specific design choices

- Defaults are 80% pressure, 16% retained tail, a derived summary budget (`min(R/2, span/2)`), one additional pressure reduction and one overflow recovery. Pruning is enabled in the integrated default policy and can be disabled independently. The pruning gate is 8,192 characters with a 4,096/1,024 head/tail split — the same floor Hermes uses proactively — so a genuinely large result is reduced before a summary is paid for; the previous 80 KiB gate sat about 100x above the measured median tool result and never qualified. Only results outside the retained tail are candidates, the newest exchange and every failed result stay whole, image and other non-text blocks are never rewritten, and each reduction records its character and token accounting. All controls are available per exact model in Settings; there is no competing route-pattern allowlist.
- Aworkit freezes the acting route and capacity at Chat creation. It can measure before the first request; Harness waits for its first durable dynamic route. An explicit frozen summary target overrides the acting model. Aworkit has no separate dynamic route or agent fallback to guess at execution time.
- Complete parallel tool exchanges are atomic surface units. Keeping or removing that unit preserves calls, results, error flags, ordering and opaque provider continuation metadata together. This implements balanced boundaries using Aworkit's existing exchange representation.
- **A compaction re-emits durable state instead of trusting the summary for it.** After a committed replacement the runtime appends one generated state block derived from the same records the tools wrote: the live Chat goal, the Run's task list, and the files this Run has already read or changed. Summaries are lossy, so the objective, the plan and prior reads must not depend on the summariser keeping them. Generated state is never pinned as user direction and an older copy is dropped rather than stacked beside the new one, so repeated compactions keep exactly one block; the block is committed inside the checkpoint document, so a restore re-reads it.
- Byte pressure can trigger reduction earlier than the model token threshold. Auxiliary requests use a bounded 768 KiB source. Provider responses (including reasoning, tool arguments and summaries), graph output, tool-call counts, individual exchanges and durable outcome commits have no application-imposed size/count ceiling. Large outcomes retain their status and full evidence. Model-facing tool previews and context compaction remain separate from durable originals. An irreducible final unit or unavailable/failed summary never authorizes deleting evidence or an unchanged overflow retry.
- **No request-size ceiling.** A Chat turn, an Agent exchange set, a model-call node, a subagent request, an approval review and the conversation replayed to the provider are all governed by the model's own context window and the provider, never by an Aworkit byte budget. `MAXIMUM_PROVIDER_REQUEST_BYTES` (32 MiB) exists only as a runaway guard against an unbounded in-memory payload and is far above any conversation a model can accept; it is never a routine limit. Conversation history is never trimmed or dropped to fit a bound.
- **A provider failure never ends a Run.** Every provider failure that is not caller cancellation — transport, timeout, an interrupted stream, an unsupported tool call, a provider refusal, a context rejection that compaction could not reduce, and a provider rejection of the auxiliary compaction request itself — is reported to the model as a recovery notice on the same frozen route, and the Agent continues. The recovery budget (32 reports) exists only so a genuinely unreachable provider is eventually surfaced instead of retried forever; the last report tells the model to close out, so a normal task ends with an answer rather than a hard error. Cancellation and an unreachable provider are the only stops the authority owns. An explicit **Compact context** command is the user's own action, so its provider failure settles as that node's failed outcome rather than becoming model context.
- New Chats freeze compaction version, policy, capacity and summary binding. Pre-upgrade Chats preserve their original frozen authority serialization and invocation identities: default byte-pressure, canonical overflow and manual maintenance remain available; exact token-pressure configuration and optional summary routing are adopted in a new Chat. Settings changes never silently alter an existing Chat's model or credentials.
- Source fencing is intentionally stricter than Harness's selected-span append tolerance: a conflicting context revision rejects the summary. The desktop core serializes maintenance and queues input, so an unrelated user append cannot race a valid maintenance commit.
- Full bounded context documents are immutable revisions in the existing semantic store, with source identities and exchange cursors. This does not introduce another conversation archive, replay tools, or mutate prior requests. The context panel remains the existing editor; no separate `/compact` parser or model-visible compaction tool is introduced.

## Compaction budget model (2026-09-25)

Every compaction budget is a fraction of the model's **effective window** `W' = contextWindow − maxOutputTokens`. The provider reserves its output space out of the same window, so the reservation is subtracted before any ratio applies; a model with no configured maximum output reserves nothing.

| Budget | Value | Source |
| --- | --- | --- |
| Trigger `T` | 0.80 W' | `thresholdRatio`, unchanged |
| Retained tail `R` | 0.16 W' (or explicit `retainTokens`) | `retainRatio`, unchanged |
| Carried user turns `U` | `retainRatio/2` = 0.08 W' | derived |
| Summary cap `S` | `min(R/2, shadowed span/2)` | derived |
| Fixed context `F` | system + tool schemas + retry notice | measured per request |

At the 16% default a replacement is at most `R + U + S` = 32% of the window, leaving 0.48 W' of work between compactions: 1,048,576 → 167,772 / 83,886 / 83,886; 262,144 → 41,943 / 20,971 / 20,971; 65,536 → 10,485 / 5,242 / 5,242.

**Why `R/2`.** One compaction can free at most `T − R = 0.64 W'`. Spending an eighth of the window on the summary keeps a 4:1 ratio of freed context to replacement cost, so the framed-summary shrink check is never marginal, and the target post-compaction occupancy is exactly `1.5 × R` from a single slider. The same share bounds the user turns carried verbatim, which lands on the same number Codex reached empirically (its flat 20,000-token user budget is 7.75% of its 258,144 window). `S` is additionally clamped to half the span it replaces, so a committed compaction always halves what it shadows regardless of the window.

**Why the summary budget is derived rather than configured.** The removed `maxTokens` setting read as a maximum while behaving as a minimum: the runtime used `max(retention, maxTokens)`, so above a 51,200-token window the setting was inert and the real cap was 16% of the window, up to 167,772 tokens at 1M — 25% of a 32k window and 0.8% of 1M. Frozen Chats that still carry `maxTokens` deserialize through a legacy serde sink and the value is ignored.

**Why 64,000 tokens is the minimum window.** The invariant is that after one compaction occupancy is `R + U + S + F` and must stay below the trigger with a quarter of the window left to work in, i.e. `F ≤ T − R − U − S − 0.25 = 0.23 W'`. With a conservative `F` of 15,000 tokens for a large MCP tool catalog that fails below roughly 64k. Below the floor, or when the measured `F` leaves less than a quarter of the window, automatic pressure compaction is disabled and one diagnostic names the reason; manual compaction and provider overflow recovery remain available, and the Chat stays usable. Hermes ships the same floor as a hard reject (`MINIMUM_CONTEXT_LENGTH = 64_000`); a 32k declared window is refused rather than allowed to oscillate at the trigger.

`context.compacted` records the whole decision: `shadowedUnits`, `shadowedTokenCount`, `pinnedUnits`, `pinnedTokenCount` and the derived `maxTokens` the summary call was authorised with.

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
