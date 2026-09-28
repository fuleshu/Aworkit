<!-- Generated Adashi agent workflow; do not edit this copy. -->
# Adashi workflow

This workspace is an Adashi project. Adashi holds its formal design, tasks, QA jobs, rules and
project memory over the MCP tools `adashi_design`, `adashi_tasks`, `adashi_qa`, `adashi_rules`,
`adashi_memory`, `adashi_intents`, `adashi_grep` and `adashi_help`. Lifecycle injections carry
project-specific instructions and current project context; this file is the shared workflow.

## Classify the request

Classify the request intend as exactly one of:

- `general`: discussion, explanation, investigation or operational help with no design deliverable
  and no code edit.
- `design`: architecture, planning, review of an approach, or discussion-only technical design. Do
  not change code unless the user explicitly switches to implementation.
- `implementation`: code creation, modification, tests, builds, migrations or generated files —
  anything expected to change the project.

## Call every lifecycle hook

Call `run.start` before the overall request, `task.start` before each concrete task, `task.end`
before each task is complete and `run.end` before the final response:

```json
{"projectName":"<configured project name>","operation":"get_rule_injections","intend":"general | design | implementation","hook":"run.start | task.start | task.end | run.end"}
```

Treat every nonempty `injectionPrompt` as active instructions for that hook, even when `rules` is
empty; apply it once before continuing. `rules` and `sections` are metadata only, and
`status: "empty"` means no instructions apply. Call every hook even when sections are unchanged;
clients may cache sections by id and `contentVersion`. For multi-task requests, use the same
run-level intend for each task unless the user clearly changes the nature of one task. A failure
because the MCP surface is not configured is not fatal: continue without Adashi and mention the
limitation only when it affects the requested outcome. Any other failure is real: report it rather
than working around it.

## Context is already injected

`run.start` already carries the memory protocol, the bounded project memory summary, and for
`design` and `implementation` the hook prompt and a bounded design index. Read that first and
retrieve only what it does not cover.

## Write discipline

- Read the exact contract with `adashi_help` before an unfamiliar operation; never submit an
  incomplete write to discover its parameters.
- Every mutation needs a unique `operationId`, reused only for an identical retry. Task, QA, rule
  and memory updates use the `expectedVersion` from retrieval, never the project revision. Design
  writes use document `readToken`s.
- A rejected write is not persisted. Correct the reported parameters and submit a new call, and
  never report completion for a rejected write.
- Design conclusions belong in the design model, not in chat notes or project memory.

## Read a skill on demand

These documents are deliberately not loaded here. Read the one that matches the work before doing
it: call `adashi_help` with just that `skill` name (no project or write required).

| Skill | Read it when |
| --- | --- |
| `design-authoring` | creating or changing C4 elements, relationships, UML, bindings or mockups |
| `markdown-documents` | writing Markdown design prose, generated docs or AGENTS.md blocks |
| `task-workflow` | creating, updating, finishing or closing a task, or resolving a task number |
| `qa-jobs` | creating, changing or running a QA job |
| `retrieval` | searching design, tasks or memory, or narrowing a broad grep |
| `memory` | a prior decision, constraint or blocker is needed, or writing a handover |
| `write-recovery` | an Adashi write was rejected and the repair is not obvious |

`adashi_help` with no `tool` and no `skill` lists the tools and skills. `adashi_help` with a `tool`
and `operation` returns that operation's exact schema, example and workflow.

Project-specific instructions always win over this shared workflow. Generated architecture blocks
in instruction files are the model's own statement of what a folder owns; change the model, never
the block.
