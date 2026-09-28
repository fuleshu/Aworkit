# DSH Standard rework — change note

Date: 2026-09-28. Adashi task #156 (finished).

## Why

The chat analysis (see `../dsh-standard-parity/REPORT.md`) showed that DSH `standard`
mode has no planner while the Aworkit "DSH Standard" workflow ran a mandatory tool-free
`Plan` model call before every Agent turn. The name therefore described something DSH
does not do. The plan-first documents keep their behaviour but get an honest name; a new
"DSH Standard" matches DSH's actual default shape.

## Installed workflow library

| document id | name before | name now | version | graph |
|---|---|---|---|---|
| `workflow.dsh-standard` | DSH Standard | **Planner Standard** | 14 → 15 | Input → Plan → Agent → Output → Wait |
| `workflow.dsh-standard-plan-review` | DSH Standard (Plan review) | **Planner Standard (Plan review)** | 8 → 9 | Input → Plan → Approve → Agent → Output → Wait |
| `workflow.dsh-standard-agent` | — | **DSH Standard** | new v1 | Input → Agent → Output → Wait |

Document ids were deliberately **not** changed, so existing chats that reference
`workflow.dsh-standard` still resolve to the (now renamed) plan-first document. The new
faithful workflow got a new id, `workflow.dsh-standard-agent`.

Store: `~/.local/share/com.aworkit.desktop/runtime/documents/workflows/`

- manifest: `manifest.json` (entries bumped as above)
- new bodies: `a20ea613…` (Planner Standard v15), `0041b925…` (Planner Standard (Plan review) v9),
  `9bf8bfaa…` (DSH Standard v1)

## New "DSH Standard" contents

Graph: `input.1 → agent.1 → output.1 → wait.1`.

The `agent.1` node is a verbatim copy of the installed planner's Agent node, so it keeps
the profile's enabled tool set and compaction policy unchanged:

- 33 tool bindings = all 34 native tools minus `tool.subagent_codex` and
  `tool.subagent_claude_code` (DSH disables those preset rows by default too), plus the
  profile's configured MCP server `mcp:mcp.f8a8cadbbebf48f1b7adaf0e8098bcbc`
- compaction: `{auto, pruneToolResults, thresholdChars 81920, headChars 73728, tailChars 4096}`
- `modelTierId: tier:balanced`, `reasoningEffort: high`

Agent instructions — the DSH `standard` persona adapted for Aworkit:

```
You are an AI agent powered by Aworkit.

You are a coding agent powered by the selected model.

Your working directory is the current Chat project workspace.
```

Mapping from DSH: the `dsh-system-prompt` identity line
(`You are an AI agent powered by DeepSeek Harness.`) becomes
`You are an AI agent powered by Aworkit.`; the preset persona prefix
(`You are a coding agent powered by the {{model}} model.`) cannot interpolate a model in a
workflow document, so `{{model}}` becomes `the selected model`; the preset persona suffix
(`Your working directory is {{cwd}}.`) becomes the Chat project workspace line that
Aworkit also states in its own project message.

No plan step, no plan-mode text: DSH standard mode plans through `todo_write`/`create_goal`
while it works, and plan mode is an opt-in session mode that Aworkit has no toggle for.

## Verification

- Aworkit's own validators, run in Vitest against the installed bodies:
  `validateWorkflow` → no issues; `assessNativeWorkflow` → `executable: true`, no issues,
  for all three documents (plus the shipped-template calibration baselines).
- Python contract mirror `~/aworkit-workflows/validate_aworkit_workflow.py` → all PASS.
- Store readback: manifest hash and document `schemaVersion` verified for every entry.

## Notes / not done

- `Planner Standard (Plan review)` still binds 32 tools; it is missing only the MCP server
  ref that was added to `Planner Standard` after the review variant was last saved. Left
  untouched because the request was a rename. Say the word to sync it.
- Aworkit was not running, so the store was edited directly under the repository locks.
  The library is read at startup: restart Aworkit to see the new names and workflow.
- The old portable copies and the superseded generator moved to
  `~/aworkit-workflows/superseded/`.

## Reproduce

```bash
python3 analysis/dsh-standard-rework/build_workflows.py
```

The script reads the installed planner/review bodies, renames them, builds the new
document from the installed Agent node, validates everything, installs under the store
locks, and writes portable copies.
