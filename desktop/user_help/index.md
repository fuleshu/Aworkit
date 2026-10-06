# Aworkit documentation

Welcome to **Aworkit** — the Agent Workflow Toolkit. Aworkit is a free,
open-source desktop app where you design AI-agent workflows visually and run
them in a familiar chat window, with full visibility of everything that happens.

Every page in this panel is a Markdown document. Links between documents open in
place; use the **Back**, **Forward** and **Home** buttons at the top to move
around, and **Close** (or `Escape`) to return to the app.

## The interface

Aworkit is one window: a navigation pane on the left, a main surface in the
middle and a status bar along the bottom.

| Area | What it is |
| --- | --- |
| **Navigation pane** | Projects and Chat history, and the switch between **Chat**, **Workflows**, **Management** and **Settings**. Start a new Chat here or select, pin, fork and delete an existing one. |
| **Chat** | The main surface. Every Chat runs exactly one selected workflow from start to finish. |
| **Composer** | The input at the bottom of a Chat. Choose the workflow, project and approval mode, attach images, then send. |
| **Run details** | The inspector on the right of a Chat. Live token usage, cache hit rate and a complete execution log of every model call, tool call and sub-agent. |
| **Workflow editor** | The drag-and-drop canvas under **Workflows**. Add nodes, connect and configure them, and import or export workflow documents. |
| **Settings** | Providers, model tiers, credentials, tools, tool plugins, MCP servers, external agents, ComfyUI, projects, approvals, data and appearance — all in one surface. |
| **Status bar** | The strip at the bottom of the window. Shows connection and activity state. |

## Getting started

Add a model provider, map the portable model tiers, and you are ready to send
your first message.

[Getting started: providers and model tiers](getting-started.md)

## Ready-to-use workflows

A fresh Aworkit profile already includes three workflows — **Simple**,
**Standard** (the default) and **Planer**. Four more example workflows ship with
the repository and can be imported when you want them.

[Workflows](workflows.md)

## What Aworkit can do

- **A chat is a workflow.** When you start a chat you pick a workflow — a graph
  of steps such as *model call → tool → condition → approval → answer*. A simple
  chat is a few steps; an advanced one can branch, loop, call tools and delegate
  to sub-agents.
- **You design workflows visually.** Workflows are plain, portable JSON
  documents edited on a drag-and-drop canvas. Export them, diff them, share
  them — there is no hidden configuration.
- **Your models, your choice.** Aworkit is model-location-neutral: a local
  model, a hosted API (OpenAI-compatible or others) or an external coding agent
  are all equal citizens. Run fully offline, fully hosted, or any mix.
- **Everything stays on your machine.** Aworkit is a desktop application, not a
  web service. Chats, workflows and settings are stored locally; credentials go
  into the operating system's credential store.
- **Nothing is hidden.** Every model call, tool action, approval and error is
  recorded and inspectable, with token usage, timings and the raw data behind
  each step.

[Feature overview](features.md)

## Tools

Built-in file, shell, Python, web and delegation tools, and how to bind them to
a workflow.

[Tools](tools.md)

## MCP servers

Connect any Model Context Protocol server and use its tools inside a workflow.

[MCP servers](mcp.md)

## Plugins and skills

Installable tool plugins that bundle MCP servers with optional skill folders,
plus the Markdown skills the agent loads on demand.

[Tool plugins and skills](plugins.md)

## ComfyUI

Turn a running ComfyUI server and your own API-format workflows into native
image-generation tools.

[ComfyUI](comfyui.md)

## Optional extras

Every installation bundles four example workflows and the FFmpeg plugin, and
copies them into an Aworkit folder inside your documents folder. Learn where
they are and how to install them.

[Example workflows and the FFmpeg plugin](optional-extras.md)

## Further reading

The repository README and the deeper design documents.

[Further reading and deep dives](further-reading.md)
