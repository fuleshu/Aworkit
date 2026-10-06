# Workflows

A workflow is a graph of steps that decides how a request is processed. When you
start a Chat you select one workflow, and the whole Chat runs that graph — so a
simple chat is a few steps and an advanced one can branch, loop, call tools and
delegate to sub-agents.

## Included workflows

A fresh Aworkit profile already contains three ready-to-use workflows:

| Workflow | What it is |
| --- | --- |
| **Simple** | A minimal input → agent → output loop. Good for a quick, tool-free chat. |
| **Standard** (the default) | The full standard agent: planning, the tool loop, approvals, sub-agents and memory. Start here. |
| **Planer** | A planning-first workflow that settles a structured plan before producing the answer. |

## Example workflows

Four more workflows ship in the repository under `desktop/workflows/examples/`.
They are import-only: they are meant to be picked deliberately, not switched on
for you.

| Workflow | What it demonstrates | Needs |
| --- | --- | --- |
| **Triage Router** | Answers simple questions on a fast model and routes anything needing current facts through an approval to the tool-enabled agent | a configured model |
| **Evidence Brief** | Starts a web search while a plan decides whether evidence is really needed, then answers with sources | web search (on by default) |
| **Iterative Planning** | Refines a structured plan until no open questions remain, then executes the settled plan | a configured model |
| **Delegated Code Review** | Scans the workspace for `TODO`/`FIXME`/`HACK` markers, asks approval, then hands an independent review to an external coding agent | a connected Codex or Claude Code target |

How to install them, and where the files live, is on
[Example workflows and the FFmpeg plugin](optional-extras.md).

## Selecting, importing and editing

- The composer lists your saved workflows; the selected one is validated before
  the first message and frozen for that Chat.
- In the **Workflows** view, the canvas lets you add nodes, connect them and
  configure the selected node in the properties panel.
- Use **Import** to load a `.aworkit.json` document and **Export** to save one.
  Imported examples are ordinary workflows you can rename, edit and re-export.

The executable node types are **Chat Input, Model Call, Agent, Tool, External
Agent, Condition, Bounded loop, Parallel, Approval, Chat Output, Wait for Input
and Completion**, so a graph can express classification, routing, iteration,
parallel work, user gates and delegation.

> **Note:** Example files live in the source repository rather than inside the
> installed app. See
> [Example workflows](https://github.com/fuleshu/Aworkit/tree/main/desktop/workflows/examples)
> to download them.

## Changing models without touching workflows

Workflows reference models and tools by portable logical names — a model tier
such as `tier:balanced` and tools by id. Swap the provider or model behind a tier
in Settings and every workflow keeps working unchanged.

Deep dive: [design work in the repository](further-reading.md).
