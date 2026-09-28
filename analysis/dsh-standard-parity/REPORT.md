# Does Aworkit's "DSH Standard" workflow behave like DSH `standard` mode?

**Question asked:** analyse chats in Aworkit and chats in DSH to verify the DSH Standard
workflow really works like DSH standard mode. The suspicion: *"I do not see DSH using a
similar planner as the DSH Standard workflow in Aworkit."*

**Verdict: the suspicion is correct.** DSH `standard` mode has no planner step at all.
Aworkit's DSH Standard workflow inserts a mandatory `Plan` model call in front of every
Agent turn. That is an Aworkit-only construct, and it is the single largest structural
difference between the two.

The planner is not useless — its output does reach the Agent on most turns (88 % of agent
model calls across the 18 chats) — but it is injected on the *per-turn context* channel
rather than as a durable plan, so whether it is still in context is decided by compaction.
In compaction-heavy runs it is folded away after a handful of turns and the rest of the
work proceeds without it.

---

## 1. What each side actually is

### Aworkit — installed `workflow.dsh-standard`, document version 14

Graph (`input.1 → plan.1 → agent.1 → output.1 → wait.1`):

| node | type | key configuration |
|---|---|---|
| `plan.1` Plan | `model_call` | `outputContract: "plan"`, `maximumTokens: 2048`, `reasoningEffort: high`, 1788-char planning prompt, **no tools** |
| `agent.1` Agent | `agent` | 33 tools, `reasoningEffort: high`, compaction overlay, 2-line persona |

Source of truth:
- shipped template: `desktop/workflows/default-workflows.json` (`standard-agent`, same shape)
- installed body: `~/.local/share/com.aworkit.desktop/runtime/documents/workflows/bodies/a15594c387edfb906aeb6a3b31e4c023a3a63b869518081a6deb25ad54c3fdf1.json`

The `Plan` node is a plain `model_call` with no `toolIds`, so the engine takes the
`definitions.is_empty()` path (`graph_pass.rs::run_agent`) and issues a **single text turn
that cannot call any tool**. The prompt even says so: *"This step cannot call tools:
the downstream Agent performs every read, search, static analysis, and check."*

### DSH — `standard` preset

Source: `packages/preset/agent-presets/presets/standard/agent.cordis.yml`

- No planner row. The model-facing rows are `plan-mode` (inside the `planning` group,
  lines 101–125), `tool-todo`, `tool-goal`, the subagent family, `workflow-ptc`, etc.
- `plan-mode` is a **session mode**, not a pipeline step. Its section text says
  *"You are in plan mode… Explore first. Use non-mutating reads, searches…"* and it ends by
  calling `exit_plan_mode` for human approval. The tool catalog is deliberately unchanged
  across modes.
- Planning during normal work is expressed live through `todo_write`
  (*"Use it to plan multi-step work and show progress"*) and `create_goal` for long
  objectives. There is no up-front plan call.

## 2. Evidence from the actual chats

### 2.1 Aworkit chat store

`~/.local/share/com.aworkit.desktop/runtime/history/aworkit.sqlite3`
(extracted with `extract_aworkit.py` → `aworkit_runs.json`)

18 chats used `workflow.dsh-standard`, spanning document versions 1 → 14.

| observation | result |
|---|---|
| chats with a `plan.1` span | 18 / 18 |
| plan node produced a structured plan (`goal`, `openQuestions`, `evidenceNeeded`, `toolOrder`) | 17 / 18 (v1 failed with `provider transport failed` after 33 retries) |
| plan output size | 3,205 – 25,564 chars |
| plan output tokens | **361,352** |
| agent output tokens | 1,648,419 |
| **plan share of all output tokens** | **≈ 18 %** |

The v1 workflow (chat `f27ac9d0eda3`) is worth noting on its own: the mandatory Plan call
retried 33 times over ~300 s and then failed the entire run — a pre-step that can kill the
turn before the Agent ever starts.

### 2.2 The plan does reach the Agent — on the per-turn context channel

This is the part that makes the behaviour feel inconsistent rather than simply different.

`graph_pass/context.rs::agent_turn_context` wraps the plan as:

```
Additional context from earlier graph steps (generated context for this turn, not a user instruction):
{"goal": ..., "openQuestions": [...], "evidenceNeeded": [...], "toolOrder": [...]}
```

with `after_exchanges: 0`, and `model_tool_loop.rs:737-749` chains that
`initial_context` into **every** provider request. The OpenAI adapter appends
`after_exchanges == exchanges.len()` contexts after the exchanges
(`provider_tools/openai.rs:144-148`), so on turn 1 the plan lands right after the user
message as an extra `user`-role message.

So the Agent *does* see the plan. But because it is pinned at `after_exchanges: 0`, the
first compaction that folds exchange 0 drops it (`compaction/runtime.rs:303-309`:
`retain(|m| m.after_exchanges > snapshot.through)`).

Measured per-chat persistence, counting only model calls owned by the `agent.1` subtree
(the plan node's own calls excluded) and matching each chat's first-turn plan:

| chat (suffix) | agent model calls | calls carrying plan | share |
|---|---:|---:|---:|
| `c4f179ec92b7` (Tetris, v12) | 125 | 3 | 2 % |
| `1c8b65bad3e7` (Tetris, v12) | 57 | 20 | 35 % |
| `01bec2a3ca30` (Tetris, v12) | 108 | 85 | 79 % |
| `90c3a2a01bcd` (task, v13) | 18 | 16 | 89 % |
| 13 other chats | — | all | 100 % |
| **total** | **1,473** | **1,289** | **87.5 %** |

Multi-turn chats re-run the plan node on every user turn (e.g. `4ccbad…` has 3 plan
spans), so each turn adds its own plan context; the 87.5 % figure measures the first-turn
plan and understates the total.

So the plan is *usually* present, but not reliably: it is pinned at `after_exchanges: 0`, so
whether a given plan survives depends on where compaction folds. In the v12 Tetris run it
was present for the first 3 agent calls and then gone for the remaining 122 — the
compaction event at `seq 3537` records `through: 3`, `trigger: byte-pressure` — while the
agent went on to build and verify the whole game without it. The plan's influence is
therefore real but compaction-dependent, not a stable property of the run.

### 2.3 DSH session store

`~/.dsh/sessions/**/session.v3.jsonl.zstd` (extracted with `extract_dsh.py` →
`dsh_sessions.json`)

25 sessions with `agentPreset: standard` (20 of them in the Aworkit workspace).

| observation | result |
|---|---|
| sessions whose system prompt contains a planning prompt | **0** |
| sessions that entered plan mode (plan-mode section in a system message) | **0** |
| sessions using `todo_write` | 8 |
| sessions creating a goal | 6 |
| `exit_plan_mode` present in the tool catalog | yes (opt-in) |
| `todo_write` present in the tool catalog | yes |

A representative run (`session-7d26a3ac…`, "implement tasks 131–136") starts by reading the
task records and the repo, then at step 5 creates a goal and calls `todo_write` — and keeps
the list live:

```
seq    72 (  1%) n=6 done=0 inprog=1
seq  1151 ( 18%) n=6 done=1 inprog=1
seq  2502 ( 41%) n=6 done=4 inprog=1
seq  3158 ( 52%) n=5 done=0 inprog=1   ← next task batch
seq  4883 ( 80%) n=6 done=0 inprog=1
seq  6053 ( 99%) n=4 done=4 inprog=0
```

Every string match for *"You are planning the request"* or *"You are in plan mode"* in the
DSH sessions is inside tool output or assistant reasoning where the agent was **reading the
Aworkit database / preset file** — never a DSH system prompt.

## 3. Side-by-side

| axis | DSH `standard` | Aworkit DSH Standard v14 | same? |
|---|---|---|---|
| Up-front plan | none | mandatory `Plan` model call | **no** |
| Planning mechanism | `todo_write` (live) + opt-in plan mode | structured JSON plan node + `tool.todo` | **no** |
| Plan can explore repo | plan mode: yes (reads/searches) | no — plan node cannot call tools | **no** |
| Plan durability | plan-mode plan is a normal approved message | per-turn context at exchange 0; survives in most chats (88 % of agent calls) but compaction-pruned in others | **no** |
| Plan approval gate | `exit_plan_mode` (in plan mode) | none in default workflow; separate "Plan review" variant exists but **0 chats used it** | **no** |
| Plan cadence | created when mode is entered | every user turn of every chat | **no** |
| Persona | `You are a coding agent powered by the {{model}} model.` | `You are a coding agent powered by the selected model.` + project message | ~yes |
| Tool catalog | bash/pwsh, fs, fs-search, jobs, skill, todo, goal, web, subagent + fork + control, workflow, ask-user, present | 33 tools incl. goal, ask_user, subagent/fork/list/message/cancel, jobs, MCP | **yes, close** |
| Background jobs | `tool-jobs` | `tool.job.*` + `tool.shell.start` | yes |
| Goals | `command-goal` + `tool-goal` | `tool.goal` (12 uses in store) | yes |
| Compaction | `compaction-basic` + tool-result pruner | agent compaction overlay | ~yes |
| Todo updates | progressive, many per session | 0–6 per chat, often one mid-run write and a stale closing write | **no** |

## 4. Root cause

Two design decisions in the workflow produce the mismatch:

1. **The plan was modelled as a graph node, not a mode.** DSH's plan mode is a state the
   *user* enters; it keeps the tool catalog stable and gates implementation on an approval.
   Aworkit encoded only the *text* of that mode (the prompt is visibly adapted from the
   `plan-mode` section) into an always-on `model_call`. So the workflow runs a plan phase
   DSH never runs by default — and, being a `model_call` with no tools, it plans blind,
   which is the opposite of plan mode's *"explore first"* instruction.

2. **The plan is delivered on the per-turn context channel.** `agent_turn_context` is built
   for *"generated context for this turn"* at an exchange anchor, and the compaction retain
   rule prunes it once that anchor is folded. A plan is an artifact of the run, not of one
   turn, so the channel is the wrong one. The visible symptom is a plan whose presence
   depends on compaction — present for the whole run in most chats, gone after three calls
   in the long Tetris run — which is a plausible source of the "it feels different /
   inconsistent" impression.

The original gap report (`~/aworkit-workflows/DSH-Standard-gap-report.md`) already graded
this `plan-mode → model_call with outputContract: plan` as **"Partial — structured plan
only; no mode toggle and no tool-catalog-stable approval gate."** The chat evidence shows
the practical consequence of that "partial": the plan is an unapproved, blind, per-turn
guess rather than a grounded, human-approved artifact, and it can fall out of context
mid-run.

## 5. Options (not applied — no workflow or application files changed)

1. **Drop the plan node** for the default workflow (make DSH Standard a plain
   `input → agent → output → wait` graph, like `Simple Chat` but with the full tool set).
   This is the closest match to DSH `standard` as actually used. Planning then comes from
   `tool.todo` / `tool.goal`, as in DSH.
2. **Keep the plan node but make it durable** — e.g. teach the engine to anchor a plan
   context so it survives compaction, or have the Agent node fold its incoming graph input
   into the durable conversation instead of the per-turn context channel.
3. **Switch the default to `DSH Standard (Plan review)`** when a human-gated plan is
   actually wanted; it is installed (v8) but has never been used in a chat.
4. **Make the plan node faithful to plan mode** — allow it to call the read/search tools
   so it can ground itself, and keep the approval gate. Today it cannot inspect anything.
5. **Fix the todo cadence** by putting explicit live-update guidance in the Agent node's
   `instructions`, so the list reads as a plan rather than a closing report (Aworkit's
   `tool.todo` manifest already says to keep it current; the node instructions do not).

## 6. Reproduction

```bash
cd /home/timofl/src/Aworkit/analysis/dsh-standard-parity
python3 extract_aworkit.py > aworkit_runs.json   # per-chat plan/agent analysis
python3 extract_dsh.py      > dsh_sessions.json  # per-DSH-session analysis
```

Both scripts are read-only against
`~/.local/share/com.aworkit.desktop/runtime/history/aworkit.sqlite3` and
`~/.dsh/sessions/**/session.v3.jsonl.zstd`.
