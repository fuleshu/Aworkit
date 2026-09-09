# Adashi: compact, relevant MCP responses and lifecycle context

Status: self-contained problem description and proposed TODO list for handoff to the Adashi project. Implementation is pending.

## Problem

Adashi supplies redundant and overly broad information to agents. Its lifecycle response repeats complete generated context in multiple fields. Task listings return full task records without pagination or a compact projection. Startup memory consists of historical handovers even when they are irrelevant to the current request, and design/implementation startup eagerly loads a broad design overview.

The goal is to return each piece of required information once, keep default responses small, and make further detail available through explicit, bounded retrieval. Stored history and default model context need separate budgets.

## Reproduction and evidence

An Aworkit Chat started on 2026-09-08 at 18:13:14 UTC asked: `are there any open tasks for aworkit in adashi?` The canonical calls and observed results were:

1. `adashi_get_rule_injections({"projectId":"Aworkit","intend":"general","hook":"run.start"})`: `generatedContext[0]` and `injectionPrompt` were byte-for-byte equivalent text, each 12,782 characters. `memoryRule` repeated the 882-character protocol already embedded in both. This was one invocation, not two memory reads.
2. `adashi_list_tasks({"projectId":"Aworkit","states":["open"]})`: successful response containing `tasks: []`.
3. `adashi_list_tasks({"projectId":"Aworkit"})`: the agent unnecessarily broadened the query. The result reaching Aworkit's output cap was 214,666 bytes, including full descriptions, completion memos, file lists and design links. Aworkit truncated it at 65,536 bytes.
4. `adashi_list_tasks({"projectId":"Aworkit","states":["confirmed"]})`: returned one full task record even though only open tasks were requested by the user.

The startup memory had 14 historical notes and no current summary. It included routine test/build reports, a tool-visibility confirmation, Git state, and superseded statements about whether automatic context compaction existed. Several notes ended mid-word or mid-sentence. The current retention implementation clips oversized legacy notes with SQL `substr`.

A separate current design startup inspection returned a 21,063-character generated design overview with 48 repeated truncation notices. That overview is also included in both generated context and the combined injection prompt. This design overview was not part of the original general-intent task lookup.

Character measurements are not token counts. Aworkit already removed MCP's separate compatibility text copy of `structuredContent`; the duplicate memory remained inside the structured payload.

## TODO: immediate fixes

- [ ] **D1 — Return one canonical lifecycle prompt body.** Remove repetition between `injectionPrompt`, `generatedContext`, `memoryRule`, and full optional-rule prompts. Keep required section text once; provide IDs, versions and section types as metadata. Apply this to general, design and implementation hooks. Define a compatible migration or versioned contract for clients that consume the existing fields.
- [ ] **D2 — Make task listing a bounded summary operation.** Default to identifying fields such as ID, number, title, state and version. Add pagination and explicit completeness information, such as filtered total and `hasMore`/continuation. Keep full descriptions, completion evidence, file lists and linked design details in explicit detail retrieval. Preserve the ability to intentionally enumerate all states.
- [ ] **D3 — Make state filtering unambiguous.** Publish supported values as schema enums, reject invalid states, and document omitted/empty filter semantics. A successful empty page must be distinguishable from an invalid query or incomplete response. An existence/count projection may be added if it simplifies common queries without multiplying overlapping tools.

## TODO: memory and generated-context improvements

- [ ] **D4 — Separate retained memory from startup injection.** Define a small injection budget independently of retention limits. Supply a concise current summary and applicable constraints; fetch historical handovers by explicit scope/query when needed. Allow routine operational requests to avoid development history. Do not assume that every `general` request is memory-independent.
- [ ] **D5 — Repair memory quality and supersession.** Review the existing notes, consolidate current state, remove routine or redundant entries from active memory, and mark or resolve superseded statements. Maintain provenance where useful. Replace clipping as a summarization strategy with complete bounded handovers; make unavoidable omissions explicit. Use the existing versioned memory APIs and agreed ownership of the canonical summary.
- [ ] **D6 — Load formal design by relevant scope.** Replace the broad automatic depth-three overview with a compact index of stable IDs and useful metadata, then retrieve explicit branches, bindings or artifacts. Preserve required design guidance. Replace per-entry truncation boilerplate with one clear explanation and usable retrieval identifiers. Keep retrieval deterministic; avoid adding speculative task interpretation to the MCP server.
- [ ] **D7 — Provide a compact lifecycle contract for clients.** Make applicable rules, generated sections and empty hooks unambiguous. Offer stable identifiers/versions so clients can avoid reinjecting unchanged content while still applying changed rules. Update protocol text and tool descriptions together. Do not silently drop required lifecycle hooks as an optimization.

## Acceptance checks

- [ ] The serialized lifecycle payload contains one copy of every generated memory/design section and required protocol instruction; rule metadata does not repeat full prompt bodies.
- [ ] A zero-match open-task query produces a small, complete result with no unrelated task descriptions or history.
- [ ] Pagination returns every requested task without duplicates or omissions; invalid filters fail explicitly, and intentional full detail remains available.
- [ ] Startup injection obeys its own budget. Related history/design can still be retrieved; routine queries do not receive the entire memory log.
- [ ] Active memory does not present superseded findings as current state, and ordinary handovers end at complete boundaries.
- [ ] Required instructions survive every projection and compatibility path. Verify both direct MCP output and the model-facing result in a supported client.
- [ ] Compare before/after output sizes using the same saved fixture. Measure complete serialized responses, not only individual prompt strings; do not equate byte savings with billed token savings.

## Implementation starting points

- `src-tauri/src/mcp.rs`: `RuleInjectionResult`, `get_rule_injections`, `format_memory_run_start_context`, `format_design_run_start_context`, `ListTasksParams`, and `TaskListResult`.
- `src-tauri/src/tasks.rs`: `Task` and `load_tasks`; list retrieval currently hydrates full matching task objects.
- `src-tauri/src/memory.rs`: retention limits, legacy clipping, canonical-summary ownership and versioned memory operations.
- Relevant fixed-hook settings, rule prompts, tool descriptions and client-facing protocol documentation.

## Ownership boundary

Aworkit owns repeated descriptions in its system prompt, unnecessary agent query broadening, tool/workflow selection, hook execution order, and its 64 KiB truncation behavior. Adashi should make its APIs economical even when a client requests a broad listing. Fixing Adashi's internal response duplication should not depend on an Aworkit-specific field-removal workaround.
