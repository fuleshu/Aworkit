# Project instructions

This project uses Adashi. Read `docs/adashi/agent-workflow.md` before starting work; it carries the
lifecycle hooks and write discipline and indexes the on-demand skills. Read a skill with
`adashi_help` (`skill` = `design-authoring`, `markdown-documents`, `task-workflow`, `qa-jobs`,
`retrieval`, `memory`, `write-recovery`) when the work matches it. If the Adashi MCP server is not
available, the same skills are mirrored under `docs/adashi/skills/`.

Project-specific rules for this repository go here; Adashi preserves everything outside its managed
block. Cross-cutting rules that should arrive at a lifecycle hook belong in Adashi rules instead.

Do not create release versions and installer bundles for testing new/changed code. Keep testing focused to the actual task.

<!-- adashi:architecture:begin -->
<!-- adashi:generated revision=8497184360559206 -->
# Architecture (generated)
Generated from the Adashi design model; do not edit, change the model.
Top layer: 14 of 99 elements, 2 of 316 relationships. Deeper detail: the adashi_design get_scope and get_bindings operations.

These responsibilities are already owned: extend them, do not duplicate.

- **Aworkit** (Software System) — Aworkit (Agent Workflow Toolkit) is a cross-platform Apache-2.0 desktop application for defining, running, resuming, and inspecting stateful AI-agent…
- **Model Provider Endpoints** (Software System) — Configured local or hosted completion and embedding endpoints. Providers return only the outputs, reasoning categories, usage, and protocol evidence t…
- **External Agent Runtimes** (Software System) — Configured lifecycle-owning agents such as Codex App Server and ACP-compatible coding agents. Each adapter declares supported progress, continuation,…
- **Trusted Extensions and MCP Servers** (Software System) — Explicitly installed and enabled subprocess extensions and configured MCP servers contributing nodes, tools, resources, prompts, providers, evaluators…
- **Operating System Services** (Software System) — Platform services used for credential storage, filesystem and process operations, notifications, native dialogs, application lifecycle, and process-tr…

Boundaries:
- Desktop Presentation -> Trusted Application Core: Submits typed commands and immutable editing intents; never invokes privileged capabilities directly

Markdown design specifications:


Adashi skills: [on-demand index](docs/adashi/skills/index.md)
Shared Adashi workflow: [current instructions](docs/adashi/agent-workflow.md)
Markdown designs: [complete index](docs/adashi/index.md)
12 more Markdown design(s); see the root index.
<!-- adashi:architecture:end -->
