# Tools

An agent node runs a model/tool loop: the model proposes a call, Aworkit checks
the frozen authority for that Chat and approval policy, executes the call and
reports the settled result back to the model. Tools are selected per Agent node
in the workflow editor.

## Built-in tools

| Tool | What it does | Approval |
| --- | --- | --- |
| **File tools** | Read, search, list, grep, edit and write files in the project workspace | reads are free; edits and writes are committed decisions |
| **Shell** | Run a host command with bounded time and output | required |
| **Python** | Run a script with the host interpreter, bounded | required |
| **Web search** | Discover sources; the built-in search works out of the box | free |
| **Web fetch / extract** | Read one page, or up to ten independently settled pages | free |
| **Skills** | Load a Markdown skill's instructions on demand | free |
| **Todo** | Maintain a run-local plan/task list | free |
| **Sub-agents** | Delegate a bounded part of a task to helper agents | required |

The exact set and its per-tool configuration live under **Settings → Tools**.
Every tool's guidance is contributed only when that tool is selected for the
node, in a deterministic order.

## Approvals

A tool that writes, runs or spends can be gated. The agent's proposal is not the
effect: Aworkit commits the approval request, suspends the run and shows an
approval card. Approving proceeds once with the exact original invocation;
declining returns a denied result and the loop continues. Decisions are
single-use and survive a restart.

## MCP and plugin tools

Any tool exposed by a configured MCP server or a tool plugin appears alongside
the built-ins once it is enabled — see [MCP servers](mcp.md) and
[Plugins and skills](plugins.md). Agent nodes can select a whole MCP server
(all enabled functions) or individual functions.

## Writing your own

A capability with a typed input, a result and a possible side effect belongs in a
tool plugin. A procedure with no typed input and no side effect belongs in a
[skill](plugins.md#skills). The authoring guide ships with the app as the
`plugin-authoring` skill, and the FFmpeg plugin is the reference implementation:
[Example workflows and the FFmpeg plugin](optional-extras.md).
