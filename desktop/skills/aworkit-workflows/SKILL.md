---
name: aworkit-workflows
description: How Aworkit workflows work and how to author or analyse one - the JSON graph model, every node type with its configuration keys, validation rules, what freezes per Chat versus what is re-resolved each pass, and how to author, validate and save a graph.
---

# Aworkit workflows

One Chat is one persistent Run of one workflow. The workflow is a
schema-versioned JSON graph; the app's workflow library is its canonical store,
and a copy can also be imported or exported as a `.aworkit.json` file.

## 1. The document

```json
{
  "schemaVersion": 1,
  "id": "workflow.simple-chat",
  "name": "Simple",
  "nodes": [
    { "id": "input.1",  "type": "input",  "label": "Input",  "position": { "x": 36,  "y": 205 } },
    { "id": "agent.1",  "type": "agent",  "label": "Agent",
      "configuration": { "modelTierId": "tier:balanced", "toolIds": [] } },
    { "id": "output.1", "type": "output", "label": "Output", "position": { "x": 470, "y": 205 } },
    { "id": "wait.1",   "type": "wait",   "label": "Wait for input", "position": { "x": 695, "y": 205 } }
  ],
  "edges": [
    { "id": "input-agent",  "source": "input.1",  "target": "agent.1" },
    { "id": "agent-output", "source": "agent.1",  "target": "output.1" },
    { "id": "output-wait",  "source": "output.1", "target": "wait.1" }
  ]
}
```

- `schemaVersion` (positive), `nodes`, `edges`: required. This build executes
  and edits version 1. Node `id`, `type` and every edge `source`/`target` must
  be non-empty strings; node ids and edge ids are unique.
- A node may carry `label`, `position` (editor layout only), optional
  `inputPorts`/`outputPorts` (arrays of objects with a non-empty `name`), and
  `configuration` (an object; the accepted keys depend on `type`).
- An edge may carry a `route` - `true`, `false`, `body`, `exit`, `fallback` or
  `feedback`. The editor writes it as the source handle; saved JSON may instead
  carry it as `configuration.route` or `sourcePort`. All three mean the same
  thing.
- Unknown fields and unknown node types are preserved by the editor and
  round-trip through save and export; a node type with no executor in this
  build blocks execution but never blocks inspecting or editing.

## 2. Structure Aworkit requires

- exactly one `input` node, which is the entry;
- at least one terminal - a `wait` or `completion` node;
- every node reachable on a path from the input to a terminal;
- acyclic, except that one declared `loop` node's single `feedback` edge may
  close its own region. Loop regions may nest at most 4 deep.

## 3. Node types and their configuration keys

Every node type in this build, with the keys the executable catalog accepts, is
in `references/nodes.md`. Read it before writing or editing a node's
`configuration` - the list there is checked against the code, so it is the
authoritative one. A node of a type with no configuration keys must have none:
adding an unknown key to a node is an error, not something silently ignored.

## 4. Freeze, and what a later turn re-reads

- The **first input** resolves and freezes the Run: the workflow document and
  its hash, the resolved model tiers, tools, external agents, workspace binding,
  budgets, and the authority manifest. The frozen snapshot is the record of what
  that Chat started with.
- The frozen snapshot is **not** the configuration authority for later passes.
  Every later turn (ordinary input, Stop/Continue, or a continuation after an
  edit) re-resolves node and edge definitions, the tool set and its interfaces,
  provider/tier selection and budgets from the **current** documents. Only Chat
  identity, the Run's authority ceiling, the workspace binding and committed
  evidence carry over.
- A resumed pass keeps its recorded position, completed node outputs, chosen
  branches and settled tool evidence, adopts the current configuration from that
  point on, and never re-runs completed work.
- Authority is the one thing an edit does not extend: a capability outside the
  Run's authorised set is decided by the normal approval path at the point of
  use.

## 5. Authoring, validating and saving

1. Author in the Workflows surface (palette, canvas, property forms) or edit the
   JSON directly; the library stays the canonical store.
2. Validate before saving. Node types, per-type configuration keys, ports,
   routes, loop regions and the structural rules above are checked; an
   executable graph must also stay under 128 KiB of canonical JSON, and a
   workflow file read or written through Import/Export is bounded at 4 MiB.
3. Save with the version the document was loaded at. A stale save is refused and
   keeps the draft; the library entry wins over a Chat's frozen copy for later
   turns of that Chat.
4. A fresh profile already has bundled templates: **Simple**, **Standard** and
   **Planer** (seeded), plus **Blank**, **Triage Router**, **Evidence Brief**,
   **Iterative Planning** and **Delegated Code Review**. Start from one of them
   to see a working graph.

Useful checks when analysing an existing graph: every node's `type` is on the
catalog list, its `configuration` has exactly the listed keys, `modelTierId` is
a `tier:<name>` reference that exists in Settings, and every `toolIds` entry
names a tool binding (`tool.<name>`, `comfyui.<tool id>` or `mcp:<server>`).
