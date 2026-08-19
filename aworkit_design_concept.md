# Base Concept

Aworkit stands for Agent Workflow Toolkit. Roughly, it should combine concepts from Codex Desktop, Hermes, ComfyUI, and DeepSeek Harness into a flexible agentic desktop app with full transparency, workflow customization, and an LLM model router that automatically routes agentic (sub)tasks to different LLM tiers for performance/cost optimization.

The tech stack should be based on Rust/Tauri, using Rig as the foundation for the agentic workflow (https://rig.rs/).

# Features

## Desktop App for Windows, Linux, and macOS

The base app should look very similar to
https://github.com/chatshellapp/chatshell-agent
in both its agent/UI architecture and its UI look and feel. Where the license permits, copy over source code where appropriate. Additional UI features should include:

- Font-size selection in settings
- Light/dark mode in settings

The overall look should stay the same and should also be similar to the Codex Windows app. The app should support multiple chats and project selection by folder.

ChatShell also serves as a reference for how to combine the Rig agentic core with a desktop app.

## Agentic Flow

It should not be a simple LLM chat app, but a full agentic flow loop like Hermes. However, it should still support multiple chats, with each chat working like a Hermes-style loop toward a goal.

## Core

The agentic core is based on Rig. However, it needs to be extended to support calls to external agents such as Codex (for example, see `@deepseek-ai/dsh-subagent-codex`). Passing MCP servers to an external agent system should also be possible.

The system should be set up as follows:

- Base model configuration, local or API-based (using Rig)
- Definition of model tiers. This should include standard tiers and custom tiers, for example:
  - Frontend model
  - Balanced model
  - Fast model

  Each tier uses one of the configured models with parameters (e.g., thinking/reasoning effort). Tiers need to have defined standard attributes, perhaps intelligence, cost, and speed values, so that they are comparable.

- Subagents:
  - Using a defined model tier
  - Calling external subagents (e.g., Codex)
- Everything follows a plugin philosophy—see DeepSeek (https://github.com/deepseek-ai/deepseek-harness). We follow DeepSeek's idea that the harness is a scoped service graph. The graph is dynamic and constructed for its purpose.
- Tools and scoped agents:
  - At the lowest level, we have tools that can be called by any agent, such as grep, file search, Bash/cmd/PowerShell, text-file editing, and web search. The tools should be created using successful open-source agents as references, such as Hermes, DeepSeek Harness, ChatShell, or other well-known, effective agentic frameworks.
  - A tool can also be created by the framework itself and be part of the Rust code.
  - Scoped subagents are agents with a defined purpose, such as software architecture design, simple coding, complex coding, web search, or file search. Each subagent is defined by its tools and model tier.
  - The tools need to handle OS-specific configurations. Each tool needs configuration variants for every OS, including all relevant specifics. Therefore, each tool must include its own tests or test suite so that correct behavior on every OS is fully ensured.
- The configuration and definition of these core parts are JSON-based and user-configurable. The desktop app must provide a UI for creating these flows visually. Complex flows may need a ComfyUI-like node system. In this way, the complete agentic harness is user-configurable and 100% transparent to the user. There is no hidden context injection. Everything is a "plugin" visible in the UI configuration.

## Agentic Workflows

The app supports multiple agentic workflows. The workflows are constructed in a ComfyUI-like UI and stored as JSON. When starting a new agentic chat, the user selects a workflow. The agentic workflow is the basic service graph, but the agent's LLM can change it dynamically according to the task.

## LLM Routing

There are basically two different motivations for LLM routing:

- Select the best LLM for the best result
- Select LLMs for the best cost/quality balance

The LLM routing itself needs to be configured in the agentic workflow. By choosing the workflow for the task, the user controls its purpose.

The routing strategy should be classification-based. The LLM classifies the task according to fixed parameters, e.g.:

```json
{
  "domain": "software_architecture",
  "scope": "repository_wide",
  "reasoning_depth": "high",
  "risk": "high",
  "verifiability": "low"
}
```

The router then routes the task according to the classification values. It needs to be decided whether there is only routing for subagent selection, where the subagent has a fixed model tier, or whether we also route at the model level to select a model tier.

A useful classifier should estimate several dimensions:

- Task family: explanation, coding, architecture, research
- Scope: local, multi-file, repository-wide
- Required operations: read, edit, execute, deploy
- Reasoning depth
- Risk and reversibility
- Ambiguity
- Available verification
- Context required
- Quality requirement

The router rule sets and outputs should also be JSON-configurable, but the app should also contain a set of default routers that work for the most common cases.
Research into the most recent LLM-routing papers should be conducted to determine the optimal routing approach based on current knowledge.

## Logging and Runtime Transparency

The app should offer an optional debug view in which the user can see, at all times, which tools and subagents are active, what the routing did, what the current context is, and what the thinking output is. This should also be logged in a database so that it can be analyzed to determine whether all tools and workflows work as intended.

# Management/Supervisor Agent

The app also needs a specific loop for management, result investigation, and optimization. It is its own agentic loop, always using the highest model tier and having access to all log data. It should be able to read and write configuration and workflow files. For example, instead of manually creating a workflow graph, the user could use the management chat to ask the LLM to create the workflows. The management agent should also be able to change the application's code. It should have knowledge of the codebase and build system, as well as a mechanism for compiling and then restarting itself with the updated code. For example, the user might ask the management agent to review an agentic task; if it finds problems with some tools that cannot be fixed through configuration alone, the agent can fix them and restart the app by itself with the corrected version.

It should also be possible to use it for self-improvement. For example, the management agent can be tasked with trying different LLM-routing strategies that it implements and benchmarks completely autonomously.

