# Workflow node showcase

The workflow editor exposes twelve executable node types, but the seeded
library used to demonstrate only five of them: `input`, `model_call`, `agent`,
`output`, and `wait`. Four new workflows exercise the remaining seven — `tool`,
`external_agent`, `condition`, `loop`, `parallel`, `approval`, and `completion`
— in concrete use cases.

Every bundled workflow is validated by
`desktop/src/workbench/workflowNodeCoverage.test.ts` (editor contract and the
native executable-catalog mirror) and by the native
`runtime::documents::tests::every_bundled_json_workflow_is_executable` test,
which also requires each one to carry a model-consuming node.

## Workflows

| Workflow | Use case | Node types demonstrated | Runtime prerequisite |
| --- | --- | --- | --- |
| **Triage Router** (`workflow.triage-router`) | Answer simple questions on a cheap tier; route anything needing evidence through an explicit approval to the tool-enabled agent. | `condition`, `approval` | None beyond a configured model. |
| **Evidence Brief** (`workflow.evidence-brief`) | Start a web search while a Plan decides whether evidence is actually required; skip tool work for general-knowledge questions. | `parallel`, `tool`, `condition`, `completion` | Web search backend (keyless fallback is on by default). |
| **Iterative Planning** (`workflow.iterative-planning`) | Refine a structured Plan until no open questions remain (bounded), then execute the settled plan. | `loop` (body/exit/fallback + feedback) | None beyond a configured model. |
| **Delegated Code Review** (`workflow.delegated-code-review`) | Scan the workspace for TODO/FIXME/HACK markers, gate delegation, then hand an independent review to an external agent. | `tool`, `approval`, `model_call`, `external_agent`, `completion` | A configured Codex or Claude Code external agent target in Settings. |

## Where the files live

- The canonical definitions are templates in
  `desktop/workflows/default-workflows.json`. A fresh profile seeds three
  workflows — **Simple**, **Standard** (the default) and **Planer** — and the
  four showcase workflows are import-only.
- Import-only because they are examples rather than everyday defaults. Only
  **Delegated Code Review** has a hard prerequisite (an External Agent node
  cannot start until a Codex or Claude Code target is configured); the other
  three run anywhere but are meant to be picked deliberately.
- Byte-identical, importable copies live in `desktop/workflows/examples/`. Use
  the editor's **Import** action to load one into an existing profile; the
  coverage test fails if an example drifts from its bundled template.

## Design notes

- **Model Call nodes see the chat transcript.** A `model_call` prompt carries the
  conversation, not only its wired input value. A classifier built on one must
  therefore say which message it judges ("classify only the final user message")
  and default to the cheap branch for stable knowledge, or an earlier
  tool-needing turn leaks into later classifications. The Triage Router prompt
  states this explicitly.
- **Conditions** inspect the single value a node carries. The router matches
  the classifier's exact scalar reply; the evidence brief tests the Plan's
  structured `evidenceNeeded` array, which is why a `model_call` with
  `outputContract: "plan"` is the structured producer.
- **Parallel** is a fork, not a join: every successor runs, and a downstream
  node waits for all of its active predecessors. The branches here are the two
  independent steps (web search and planning) that converge on one agent.
- **Loops** must declare exactly one `body`, `exit`, and `fallback` route plus
  one `feedback` edge, and their region cannot contain a `wait` or
  `completion`. The iterative planner exits when `openQuestions` is empty and
  still executes through the `fallback` route when the declared
  `maximumIterations` is reached.
- **External Agent** nodes reuse the frozen delegation binding; without a
  configured target the node fails loudly rather than silently degrading.
