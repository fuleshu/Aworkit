# Plan — Intent-Scoped Adashi Integration for the Agent Workflow

**Status:** planning only. No code written yet.
**Repo:** Aworkit (`C:\src\Aworkit`).
**Prerequisite reading:** `AGENTS.md` (the Adashi lifecycle contract) and `docs/context-compaction.md`.

---

## 1. Why this plan exists

The model driving Aworkit's agent nodes was being handed ~36 Adashi MCP tools at once, under
opaque hash names:

```
read_file, write_file, shell, python, …                       (17 native tools)
mcp__62af506f6588f1b35e2d50f94fad2b09__adashi_get_memory      (36 MCP tools)
mcp__438430061049745db57211a2d98a689c__adashi_append_memory_note
mcp__14006139c5bf3aafae1ab22436a684c4__adashi_create_qa_job
…                                                              = 50 tools per request
```

Observed damage (live chat `chat.3c386def…`, 9 model turns):

- The model **hallucinated a tool name** — `mcp__ai__write_file` — because it could see the
  `mcp__…__tool` shape but not read the 32-hex middle. The run died with
  `agent node 'agent.1' failed: provider failed: OpenAI stream ended with an unsupported tool call`.
- Its own reasoning shows the confusion: *"the repeated tool-name instability"*, *"let me verify
  which name works"*, *"the tool registry re-registers each turn"*. (The tool list was in fact
  stable at 50 tools across all turns — the model simply could not re-identify its tools.)

Root cause: **the model-facing surface is too large and unreadable.** The cure is to scope what the
model sees — both tools and instructions — to the intent of the moment, which is exactly what
Aworkit's customizable node graph exists to express.

---

## 2. Already done (do not re-plan)

- **Adashi capability grouping — DONE.** Adashi now exposes **6 capability tools**, each with an
  `operation` enum, instead of ~36 granular tools:

  | Tool | Operations |
  |---|---|
  | `adashi_design` | save, get_scope, get_by_ids, search, get_overview, get_bindings, set_element_descriptions, mockup_list_pending_revisions, mockup_get_revision_context |
  | `adashi_intents` | publish, list |
  | `adashi_memory` | get, append, update, update_rule |
  | `adashi_qa` | create_job, update_job, delete_job, list_jobs, run_jobs, list_runs, get_job |
  | `adashi_rules` | list, create, update, delete, get_rule_injections |
  | `adashi_tasks` | create, list, update, finish, delete, get |

  Verified live (2026-09-11): every listed operation was exercised and returned correct data.

- **`AGENTS.md` updated** to the new contract: `adashi_rules` + `operation: get_rule_injections`
  keyed on `projectName`; formal design through `adashi_design` operations; `adashi_grep` for
  project-wide search; generated architecture blocks between `adashi:architecture:begin/end`.

### Known outstanding Adashi-side issue (blocker for Phase 3)

`adashi_qa` `list_jobs` returns **every job with its full `latestRun.output` and `runHistory` logs**
— measured at **772 KB** for a single call, and `limit` does not apply to `list_jobs` (it is
documented for `list_runs`). A workflow that lists QA jobs would detonate the model's context.
`list_jobs` must return metadata only (id, number, name, enabled, derivedState, last-run status +
timestamp + duration), with logs retrievable per run. **Fix this before wiring QA into Phase 3.**

---

## 3. Phase 1 — Readable MCP tool names (Aworkit)

**Goal:** the model sees `adashi_design`, never `mcp__62af506f…__adashi_design`.

**Where:** `desktop/src-tauri/src/runtime/mcp_tools/names.rs` → `mcp_provider_name(server_id, tool)`.

**Today:**

```rust
// name = "mcp__" + sha256("mcp://{server_id}/{tool}")[..32] + "__" + tool[..25]
format!("mcp__{}__{suffix}", &hash[..32])
```

**Invariants that must survive (all currently covered by tests):**

- Provider-name limits: **≤ 64 bytes**, grammar `[A-Za-z0-9_-]`
  (`adashi_names_with_generated_server_ids_fit_the_provider_contract`).
- **No aliasing**: distinct `(server, tool)` pairs must not fold to the same name
  (`names_do_not_alias_after_folding_truncating_or_joining_identifiers`).
- **Frozen aliases**: already-persisted names must keep resolving
  (`portable_frozen_name`, `existing_usable_frozen_aliases_remain_exact`).
- Internal identity is separate and must not change: `mcp_internal_id`, `capability_id` (`mcp://<server>/<tool>`).

**Approach:**

1. Derive a readable **server label**: the MCP server's configured display name if available,
   otherwise a short sanitized slug of the server id.
2. Compose `mcp__<serverLabel>__<tool[..N]>`.
3. Only on an actual label collision, append a short disambiguating suffix. The hash becomes the
   **fallback**, not the default.
4. Keep the digest path intact for internal identity.

**Acceptance:**

- Adashi tools appear with readable names in a live run's evidence (`span.started` → `input.tools`).
- All three name tests above still pass (updated where they assert the new format).
- A chat frozen under the old names still resolves its tools.

**Risks:** server ids are generated (`mcp.166dddff4b6840dba8aed1edbbb9e427`), so the readable label
must come from the server's configured name — define the fallback explicitly for missing/ugly names.

---

## 4. Phase 2 — Inject instructions exactly once (Aworkit)

**Goal:** the same instruction block must not be pasted into the context repeatedly.

**Today:** the model calls `adashi_rules` at each lifecycle hook and pastes the returned
`injectionPrompt` into the conversation. Across a multi-task run that re-pastes the memory protocol,
the design index, and the design-authoring guide at every task boundary. The file-based
`workspace_instructions` plugin already renders with *"reference templates, scoped rules, dedup and
shared byte budget"* (design `uml.workflow_worker.workspace_instructions_activation`,
`uml.workflow_worker.workspace_instructions_compaction`), but **MCP-mediated rule injection bypasses
that path entirely**.

**Options:**

1. **Host-side dedup by content hash — recommended.** Aworkit already records admitted instruction
   context as `context.instructions` events. Before admitting an instruction block, hash it and skip
   when that hash is already present in the visible context. Deterministic; no reliance on the model.
2. Model node counting — fragile; models forget and drift.

**Approach:**

- Choose the interception point: either a generic "MCP tool result admission" dedup, or route the
  Adashi rule injection through the existing workspace-instructions path so it inherits dedup +
  shared byte budget.
- Respect the contract in `AGENTS.md`: a **changed** section (`contentVersion` differs) must still be
  injected — only exact repeats are skipped.

**Acceptance:**

- Two identical `adashi_rules get_rule_injections` calls (`intend` + `hook`) yield **one** visible
  block.
- A changed section (new `contentVersion`) **is** injected.
- No regression in the existing workspace-instructions dedup tests.

**Risks:** dedup must be keyed on content, not on the call, or a legitimately changed rule set is lost.

---

## 5. Phase 3 — `coding_workflow` graph with intent-scoped tools (Aworkit)

**Goal:** a workflow JSON for large software projects that uses Adashi's capabilities, with tools and
instructions scoped to the intent of the moment.

### Verified grounding (do not re-derive)

- **Composed node types** (`KNOWN_NODE_TYPES`, `desktop/src-tauri/src/runtime/documents.rs`):
  `input`, `agent`, `model_call`, `tool`, `condition`, `parallel`, `approval`, `output`, `wait`,
  `completion`.
- **`condition` is already a router executor**: `WorkerExecutorKindV1::Router`
  (`pipeline.rs`), configured by a **pure predicate** today — `{"predicate":{"kind":"always"}}`,
  `{"kind":"exists","path":"text"}` (`validate_condition_configuration`).
- **The design already specifies an LLM classifier for routers that is not yet composed**:
  `aworkit.workflow_worker.routing` + `rel.worker_routing.model_agent` — *"Requests an LLM classifier
  only when declared by the frozen router and accepts schema-valid structured classification rather
  than provider-native output."*
- **An agent node's tools come from static `configuration.toolIds`** (see
  `docs`-referenced `workflow.standard-agent`, which binds 19 tool ids).
- **Frozen authority**: the tool set is validated and frozen at Chat freeze by
  `aworkit.trusted_core.snapshot_freezer`; a running Chat's graph is immutable. Any scoping design
  must keep the grant frozen and must not let the model decide its own authority at runtime.

### Two possible shapes

**Shape A — classifier router + per-intent agent nodes** (uses the designed router)

```
input → route(condition + LLM classifier: intent) → agent.code    (code tools)
                                                 → agent.design  (adashi_design + code read)
                                                 → agent.qa      (adashi_qa + shell)
                                                 → output → wait
```

Requires composing the designed LLM classifier for router nodes, and per-route agent nodes with
different static tool sets. Note: this alone fixes the tool-surface problem **without** any runtime
tool-set computation, because each agent node simply declares fewer tools.

**Shape B — intent node + computed tool set** (no new node type)

```
input → intent(model_call, outputContract "intent") → agent.build (toolIds computed from intent)
      → output → wait
```

Requires a new `intent` output contract on `model_call` (the `plan` contract already exists as a
precedent) plus a host capability that resolves an agent node's tools from an upstream structured
output.

**Recommendation:** start with **Shape A**. It reuses node types that already exist, keeps every
agent node's authority static and frozen, and delivers the win (few tools per node) with the least new
machinery. Treat Shape B as a later refinement if a single agent node genuinely must change its tool
set mid-task.

### Work items

1. Decide the intent taxonomy and the per-intent tool sets (native code tools + the 6 Adashi
   capability tools), keeping each set small (target ≤ 20 tools per node).
2. Author `coding_workflow` JSON: intent classification → per-intent agent node(s) → review/verify →
   output → wait, with Adashi lifecycle rules injected at the right points.
3. If the intent must be model-classified (not a pure predicate), extend the router per the existing
   design (`aworkit.workflow_worker.routing`) rather than inventing a parallel mechanism.
4. Persist any design change through the `adashi_design` save operation.

### Acceptance

- The workflow loads and freezes without unknown-node or unknown-configuration errors.
- A coding request shows **only** the code tool set (+ `adashi_rules`); a design request shows
  `adashi_design`; neither sees the other's tools.
- The model never sees more than the agreed tool ceiling at once.
- A small end-to-end task completes without a tool-name hallucination.

### Risks

- **Authority**: tool scoping must not weaken the frozen grant. The set is frozen at Chat freeze.
- **Wrong intent**: if the classifier mis-routes, the node lacks a needed tool — provide a declared
  fallback route rather than failing silently.
- **QA payload**: `adashi_qa list_jobs` must be fixed (§2) before QA is wired in.

---

## 6. Sequencing

1. **Phase 1** (readable names) — independent, small, immediately reduces confusion.
2. **Phase 2** (dedup) — independent, and needed before scoping really pays off.
3. **Phase 3** (workflow) — depends on 1 and 2.

Do not start a phase before the previous one is merged. Each phase ships with its own tests.

---

## 7. Verification recipe

- Capability host: `cargo test -p aworkit-capability-host`
- Desktop: `cargo test --manifest-path desktop/src-tauri/Cargo.toml --lib`
- **Clean-rebuild caveat:** a change to a `const` value is not reliably picked up by incremental
  builds (observed live: `PROVIDER_TIMEOUT_RECOVERIES_V1 = 1 → 5` kept behaving as `1`). Run
  `cargo clean` before verifying any const-only change.
- Live check: run the app and read the model's actual tool list from the run evidence
  (`span.started` → `input.tools`). The desktop app is a GUI process — it must be started by the
  user with `pnpm desktop:dev`, not from a sandboxed agent shell.
