# Design extension — V1 Standard Agent Workflow, Executable Node Catalog, and Built-In Tool Matrix

Status: implementation contract for the "standard agent workflow" goal.
Source: formal Adashi design (workspace revision 427) + read-only audits of
`C:\src\deepseek-harness` and `C:\src\rust\chatshell-{desktop,agent}`.
Intended Adashi artifacts (to be stored when the design-write surface is available):
`uml.system.standard_agent_workflow_v1` (flow, attached to system `1`) and
`uml.system.executable_node_catalog_v1` (class, attached to `aworkit.workflow_worker`).

## 1. Goal

Aworkit ships a standard agent workflow with parity to DeepSeek Harness and ChatShell:
user input → (optional planning/context model step) → bounded model/tool agent loop with
the full built-in tool set, plan/todo tracking, web access, subagent delegation, and user
approvals — represented as an editable node graph that is the actual executable
specification, edited in the workflow editor with a complete working UI.

## 2. Reference parity summary

DeepSeek Harness (`packages/core/agent-loop/src/agent.ts`): session → turn → step event
machine; system-prompt assembly + request derivation per step; tool calls settled in
model order through pre-execute → execute → post-execute → finalize → result; no built-in
turn budget; termination = final assistant step ∧ empty inbox. Repeated
identical tool calls receive escalating advisory reminders at counts 3, 5, and 8 but are
never blocked by a turn counter. Essential tool set of the
`standard` preset: read/write/edit/glob/grep/bash|pwsh, todo_write, skill, web_search,
subagent/fork/control/report, workflow/ralph, goal tools, ask_user_question.

ChatShell desktop (`src-tauri/src/llm/` + `commands/chat/`): pre-loop search triage;
rig multi_turn(1000) with sequential tool execution; 12 rig-native tools — bash,
kill_shell, read, write, edit, grep, glob, web_search, web_fetch, skill, mcp_schema,
mcp_tool_use; path-policy security layer; no subagent/plan/todo (net-new from Harness).

## 3. V1 executable node catalog (workflow document schemaVersion 1)

Nodes carry `id`, `type`, `label`, `position`, `inputPorts`/`outputPorts` (typed ports),
and a typed `configuration` object. Unknown types/fields remain lossless.

| Node type | Executor | Configuration | Semantics |
|---|---|---|---|
| `input` | Pure | none | Entry; passes the latest user text. |
| `model_call` | Model (no tools) | `modelTierId`, `instructions`, `maximumTokens` | One completion; output text feeds downstream (planning/context). |
| `agent` | Agent | `modelTierId`, `toolIds[]`, `instructions` | The standard turn/step loop; tools bound from enabled Settings tools. It runs until the model answers or a real deadline/context failure occurs. |
| `tool` | Brokered | `toolId`, `parameters` | One settled capability invocation; result feeds downstream. |
| `condition` | Router | `predicate` (always/exists/eq/neq/and/or/not over the incoming value) | Routes true/false per edge `route` label (`true`, `false`, `fallback`). |
| `parallel` | Branch fork | none | Pure fork marker; every successor runs; downstream nodes join implicitly (a node runs once all predecessors settle). |
| `approval` | Gate | `title`, `message` | Suspends the Run; approve continues, reject fails the pass. |
| `output` | Pure | none | Collects the final assistant text into the timeline. |
| `wait` | Wait | none | Ends the pass; the Chat waits for the next input. |
| `completion` | Terminal | none | Terminal marker. |

Execution model (v1): one **graph pass** per user input. Nodes execute in topological
order with the accumulated conversation as context; the `agent` node runs the bounded
model/tool loop; the pass ends at `wait` or `completion`. Follow-up input starts a new
pass on the same frozen graph. The graph is frozen at first input (existing invariant).

Implementation note: v1 executes the frozen graph **in-process** in the desktop runtime,
which links the trusted-core, workflow-worker scheduler/agent-loop, and capability-host
libraries behind the designed boundary contracts. The standalone worker process remains
the designed target for later milestones; this is reported, not silently drifted.

## 4. V1 built-in tool matrix

Approval-free (authority-settled, read-only or run-local state):
`tool.files.read` (≤64 KiB), `tool.files.search` (≤512 results), `tool.files.list`
(glob, ≤1000 entries), `tool.files.grep` (regex, ≤512 matches, bounded files),
`tool.todo` (run-local plan/task list), `tool.web_search` (HTTPS, ≤8 results),
`tool.web_fetch` (HTTPS, 1 MiB download / 32 KiB extracted), MCP tools
(`mcp://<server>/<tool>` for enabled, core-attested servers).

Approval-required (committed user decision before effect):
`tool.files.edit` (old_string/new_string replace), `tool.files.write` (full content),
`tool.shell.host` (bounded time/output), `tool.python.host` (bounded),
`tool.subagent` (bounded delegation with read-only tool subset).

Invariants: model output is only a proposal; every call is settled through the durable
trusted-core broker with frozen authority; approval is a durable committed decision;
uncertain side-effect outcomes are never silently retried.

## 5. Approval flow

1. The agent loop requests a tool whose binding requires approval.
2. The pipeline durably commits `approval.requested` (decision id, tool, payload
   summary) and suspends the Run in phase `awaiting_approval`.
3. The timeline renders an approval card; the user sends the `approval` command
   (approve/reject).
4. Approve → the exact original invocation proceeds once. Reject → the loop receives a
   denied result and continues. The decision is single-use, TTL-bounded, and
   crash-resumed through the existing pending-effect reconciliation.

## 6. Workflow library

- Native catalog of saved workflow documents (id, name, updatedAt, default flag),
  CRUD with version-checked commits, seeding (`workflow.simple-chat`,
  `workflow.standard-agent`), and a per-profile default.
- The composer lists saved workflows and validates the selected one natively before
  first send; the first input freezes the selected document.
- The editor selects/creates/renames/duplicates/deletes workflows and shows the
  projected per-node Run status (idle/running/completed/failed) from committed node
  lifecycle events.

## 7. Editor UI completion

- Typed node catalog drives the palette, typed port handles, per-node property forms
  (model tier select, tool multi-select, turn/predicate fields, JSON parameters),
  connection type-checking, cycle detection, and native-executability validation.
- Unknown node types keep the preserved-JSON raw editor.
- New timeline cards: plan (model_call), todo (live list), web results, subagent
  (collapsed child activity), approval (with Approve/Reject actions).

## 8. Task breakdown (Adashi tasks created for this goal)

1. Design task (this document + Adashi UML/description updates when the write surface allows).
2. Workflow library and generalized v1 workflow executability validation (native).
3. General graph compiler and interpreter in the desktop runtime with per-node events.
4. Approval path for edit/write/shell/python plus new tools (list, grep, write, todo,
   web_search, web_fetch).
5. Subagent delegation tool.
6. MCP tools in the agent loop.
7. Complete workflow editor UI (catalog, property forms, library, run status, composer
   workflow selection, new timeline cards).
8. End-to-end functional QA gate for the whole goal.
