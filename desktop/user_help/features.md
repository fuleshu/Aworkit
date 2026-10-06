# Feature overview

- **Chat that runs real workflows.** A familiar chat interface where each chat
  runs one selected workflow from start to finish. The sidebar organises projects
  and history, the composer picks the workflow, project and approval mode, and
  **Run details** shows live token usage, cache hit rate and a full execution log.
- **A visual workflow editor.** Drag nodes onto a canvas, connect them and
  configure them — no code required. The node types cover Chat Input, Model Call,
  Agent, Tool, External Agent, Condition, Bounded loop, Parallel, Approval, Chat
  Output, Wait for Input and Completion.
- **Everything configured in one place.** Settings cover model providers and
  credentials, model tiers, built-in tools, skills, tool plugins, MCP servers,
  external agents, ComfyUI, projects, data retention and appearance.
- **Portable references.** Workflows name models and tools by logical tier and
  id, so you can swap a provider or model without touching a workflow.
- **Built-in tools and integrations.** File tools, shell and Python, web search
  and extraction, skills, sub-agents, MCP servers, external coding agents,
  ComfyUI image workflows and installable tool plugins — see
  [Tools](tools.md), [MCP servers](mcp.md), [Plugins and skills](plugins.md) and
  [ComfyUI](comfyui.md).
- **Approvals and transparency.** Actions that write files, run commands or
  spend money can require your explicit approval, per chat and per step. Expand
  any step to see its exact input, output and raw JSON.
- **Local by default.** Chats, workflows and settings live on your machine, and
  credentials go into the operating system's credential store.

## Where each feature is explained

| Topic | Document |
| --- | --- |
| The window and its surfaces | [Home](index.md) |
| Providers and model tiers | [Getting started](getting-started.md) |
| Built-in and example workflows | [Workflows](workflows.md) |
| Built-in tools | [Tools](tools.md) |
| MCP servers | [MCP servers](mcp.md) |
| Tool plugins and skills | [Plugins and skills](plugins.md) |
| ComfyUI image workflows | [ComfyUI](comfyui.md) |
| Optional extras to install | [Example workflows and the FFmpeg plugin](optional-extras.md) |
