# ComfyUI

Aworkit has a first-class **ComfyUI** Settings tab that turns a running ComfyUI
server and your own API-format workflows into native agent tools. There is no
bridge subprocess and no MCP server: the desktop host talks to ComfyUI in
process over local HTTP.

## Adding a workflow tool

Open **Settings → ComfyUI**:

1. Set the **endpoint** — the HTTP(S) base URL of the ComfyUI server, without
   credentials, query or fragment. Use **Test** to confirm it is reachable; the
   result reports the server's ComfyUI version when it returns one.
2. Add a **workflow tool** and point it at an API-format workflow JSON file.
3. Either fill in the name, description and parameters yourself, or use
   **Auto create** to have the model mapped to `tier:balanced` propose them.
4. Review and save. Enabled workflow tools appear as native tools on Agent
   nodes.

## What a workflow tool is

Each enabled workflow becomes one capability with the stable id
`comfyui.<tool id>`. Its model-facing name, description and JSON schema are
generated from the typed parameter list you edit, so the schema the model sees
and what execution writes cannot drift apart.

A parameter declares its name, description, JSON type, whether it is required,
an optional default or allowed values, and the exact **binding**: the workflow
node id and input name the value is written to.

**The binding is the security boundary.** A workflow tool can write only to a
node input that exists in that referenced workflow. It cannot invent a node, an
input, a model filename or a downstream capability. Parameters that are not
bound to a real input are rejected, and the two helper ids below are reserved so
a workflow tool can never shadow them.

## Authoring helpers

Two read-only native tools help while you author bindings:

- `comfyui_list_node_types` searches the live `/object_info` catalog under a
  bounded page.
- `comfyui_get_workflow` returns the API graph of one configured workflow tool
  so you can inspect its node ids and input names.

Both run without approval. The ComfyUI tab can also register an optional
workflow-authoring MCP server that gives the agent ComfyUI node and workflow
knowledge; it is an ordinary MCP entry with the normal trust, transport and
approval semantics, and it is knowledge and reads — never a second execution
path.

## Connection and local start

Reachability is a bounded `GET /system_stats`. If you configure a local
installation folder and enable it, Aworkit can launch ComfyUI with your
arguments and wait until it is ready before queueing work. Starting is always an
explicit action or an opt-in setting; a Chat never spawns ComfyUI on its own
initiative.

## Freezing, execution and evidence

- The Settings section resolves once, at a Chat's first input, so a later edit
  cannot change what a running Chat executes.
- A call is refused before ComfyUI sees it when it carries an undeclared key,
  omits a required value, passes a null, uses the wrong JSON type or selects a
  value outside the reported choice set.
- A successful run returns the produced images as immutable image evidence and
  as vision input for the model.
- An unreachable server, a workflow that produces no image, and unreadable or
  oversized workflow JSON are all reported as named failures; the run continues.

Deep dive:
[ComfyUI design document](further-reading.md).
