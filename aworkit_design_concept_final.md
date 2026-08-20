# Aworkit — Design Concept

**Aworkit** stands for **Agent Workflow Toolkit**.

## Product direction

Aworkit is an Apache-2.0 desktop application for building, running, and inspecting advanced AI-agent workflows. It combines the familiar experience of a desktop chat application with a graphical editor for complete agent harnesses. It is meant to be highly flexible and technically cutting-edge without making the normal user experience complicated.

Aworkit is model-location-neutral. Local models, hosted model APIs, and external agents such as Codex are equal first-class capability sources. A local-only installation is valid, but Aworkit does not assume that a local model exists. Hosted-only and hybrid setups are equally valid. A user may, for example, combine a local model with Codex and another hosted API, while another user may combine Codex or Claude with DeepSeek without running a local model at all.

Model providers, endpoints, credentials, concrete models, tools, MCP servers, and external agents are configured in the application. Workflows refer to those configured capabilities through portable logical identifiers. The desktop application owns orchestration, workflow execution, routing, authority, history, and transparency. Aworkit is not a backend product or a frontend for a hosted control plane.

The architecture is general-purpose and domain-neutral. The initial reference workflow families are:

- software development and repository work;
- research, comparison, and synthesis; and
- work with local files, documents, and personal knowledge.

These initial focus areas do not limit the architecture. The primary user is an individual desktop user, ranging from someone who starts with guided chat or a ready-made template to an expert who creates sophisticated harnesses. These are different depths of the same application, not separate basic and advanced products.

Aworkit targets Windows, Linux, and macOS. Built-in tools that depend on the operating system—such as file search, text editing, shell execution, and process handling—need suitable implementations and tests for every supported platform.

## Product references and reuse

ChatShell Desktop is the main reference for the familiar desktop shell. Aworkit may selectively reuse audited, license-compatible components where they already implement the same generic behavior well: the sidebar and chat shell, message and streaming presentation, provider and model setup, MCP configuration, settings, theme support, and similar application infrastructure. This is selective reuse, not a direct ChatShell fork. Aworkit keeps its own identity, workflow system, routing, persistence, authority model, and execution runtime.

Hermes is a reference for practical goal-directed agent loops, tools, and subagents. Codex and similar desktop agents are references for project-oriented chat interaction, while Codex App Server is the first rich external-agent integration target.

ComfyUI is a reference for the usability of visual node editing, a portable JSON graph, and clear handling of missing nodes or plugins. It is not Aworkit's execution model. Aworkit composes stateful, context-carrying agent harnesses rather than input-to-output inference pipelines, and ComfyUI code is not copied into the Apache-2.0 project.

DeepSeek Harness is a reference for scoped, composable harness services and extensibility. Aworkit does not adopt full application event sourcing or turn every essential desktop responsibility into a plugin. Rig is a useful Rust engine library behind Aworkit's own contracts; it is not the product architecture.

## Desktop experience

Aworkit uses a familiar desktop-chat structure, not a collection of administration dashboards.

The left navigation is ordered as follows:

1. **New Chat**.
2. A pinned **Management Chat**, with small badges for recurring errors or repairs awaiting review.
3. **Workflows**, which opens the graphical workflow editor.
4. Optional application tools added later, such as schedules.
5. **Projects**, each expandable into its project-specific chat history.
6. Non-project chat history, placed after all project histories.
7. Settings and general application controls at the bottom.

The selected Chat/Run occupies the main area. Its header shows the project or non-project scope, the active workflow, the branch or worktree when relevant, and the current status. The conversation contains ordinary messages together with useful cards for plans, tool actions, artifacts, approvals, errors, and repairs. Pause, cancel, retry, fork, continue-in-new-chat, and inspect actions appear where they are relevant rather than in a permanent expert control panel.

Before a Chat starts, the composer contains the workflow selector, attachment and context controls, and the normal send action. A project may suggest a default workflow, while the user can select another without opening the workflow editor. Once the first input starts the Chat, the selected workflow is shown as a locked identity and the composer supplies further input or stop and control actions to that same workflow instance.

Missing model-tier mappings, tools, external-agent targets, plugins, or node implementations are shown before execution with a direct path to resolve them. The unresolved workflow remains intact and editable; Aworkit does not silently remove or replace missing parts.

Expert evidence opens in a collapsible inspector instead of occupying the normal chat view. Selecting a route, node, tool call, error, or artifact in the conversation opens the corresponding evidence. The workflow editor and substantial settings pages may temporarily replace the main chat area when they need more room.

Settings cover model providers and tiers, credentials, tools, plugins, MCP servers, external agents, data behavior, and appearance, including font-size selection and light and dark themes. There is one progressively disclosed interface: a new user sees what is happening and what needs a decision, while an expert can inspect the exact workflow, context, routing, actions, and history.

The Management Chat can operate in application-wide context or in the context of a selected project. Its current scope is always visible.

## A Chat is one running harness

In Aworkit, a **Chat** is also the logical **Run** and the stored session. A Chat is not a container that creates a separate Run for each user message. It is one persistent instance of one selected workflow.

A new Chat begins as a draft while the user selects a workflow and prepares the first input. Submitting that input resolves the workflow's model targets, tools, plugins, configuration, project workspace, and authority. The resulting workflow instance is frozen for the complete lifetime of the Chat/Run.

Later user messages enter that same instance. If the workflow is busy, ordinary input queues in order unless the active workflow and integration explicitly support steering or interruption. Restarting the desktop application rehydrates the same non-terminal Chat/Run from its recorded logical state; it does not create a new Chat or replay completed side effects.

The workflow decides whether it waits for more user input or reaches a terminal state. A built-in **Simple Chat** workflow can be only:

```text
Chat Input → Agent → Chat Output → Wait for Input
```

More advanced workflows can route work, call tools and external agents, start subagents, evaluate results, run several loops, and still remain one Chat/Run.

A completed, failed, or cancelled Chat remains inspectable but no longer accepts ordinary input. **Continue in new chat** creates a new Chat/Run from explicitly selected prior context and lets the user choose its workflow. Forking from a checkpoint likewise creates a new Chat with explicit lineage to its source.

Retrying the current workflow operation creates another attempt inside the same Chat. Retrying or editing older history forks the Chat instead of rewriting history on which later events already depend. Chat ID, Run ID, and session ID therefore refer to the same Aworkit object even when an external system uses different native terminology.

The Management Chat follows this same lifecycle. It is a long-lived instance of a management-oriented workflow, not a separate hidden execution system.

## Direct project workspaces

A project is configured from a selected folder or other workspace location. A project Chat operates directly in that one resolved workspace. Its tools, subagents, and external agents receive the same workspace. Access to another configured project or workspace must be explicitly present in the frozen workflow.

A non-project Chat has no implicit project workspace unless its frozen workflow explicitly names one.

Changes made by the harness affect the selected workspace immediately. Cancelling, retrying, or deleting the Chat does not imply undoing file changes. Reversal requires an explicit tool action or an ordinary version-control or backup operation.

A normal directory, Git worktree, container-mounted checkout, remote checkout, or another prepared directory can all be the selected workspace. These are workspace choices made before the Chat starts, not separate `direct` and `isolated` workflow modes. Aworkit may provide **Create worktree** as a project-management convenience; after creation, that worktree is simply another selectable project workspace.

Aworkit does not require a per-Chat copy of the project, a hidden staging area, or a later apply-and-merge phase.

## Workflow documents and the visual editor

An Aworkit workflow is a long-lived agent harness stored as one schema-versioned JSON document. The canvas edits that document directly. JSON is the canonical workflow representation, not an export from a database, and there is no second editable or SQL copy of the workflow body.

The workflow document contains:

- node instances and their configuration;
- typed connections and harness transitions;
- canvas positions, layout, groups, and comments;
- logical references to configured model tiers, agents, tools, external agents, and subworkflows; and
- optional plugin and node requirements.

Provider setup, endpoints, API keys, and credentials do not belong in the workflow. Model-facing nodes normally reference the configured named tiers. Other capabilities use stable logical identifiers.

Opening a workflow always preserves and displays the complete graph, including unknown nodes and their original configuration and connections. A missing custom model target is visibly unresolved and offers a path to bind or create it. A missing node implementation is shown as **Missing plugin** with the available identifying information. An installed but incompatible version is distinguished from a completely missing dependency. Required unresolved references prevent execution, but they never prevent inspection, editing, or lossless export.

The first product does not need a central plugin repository or automatic installation. It only needs to explain what is missing and guide the user to plugin settings or a documented manual installation path. Importing a workflow never installs or activates code.

The built-in node vocabulary must be compact enough to understand and strong enough to create advanced harnesses. It covers these capability families:

- **Intelligence:** Model Call, Agent, Subagent, and External Agent.
- **Prompt and context:** instructions and prompt layers, context selection and transformation, evidence or memory selection, artifact references, and result integration.
- **Actions:** built-in tools, plugin tools, MCP calls, and subworkflows.
- **Harness control:** chat input and output, conditions and routers, forks and joins, bounded loops, for-each and parallel work, wait for input, retry and fallback, approvals, and completion.
- **Quality:** evaluators, verification, gates, and aggregation of several results.

The exact set can evolve, but the architecture must support branches, several and nested loops, parallel work, retries, approvals, evaluation, and advanced agent harnesses from the start. Loops and delegation have explicit limits and budgets. Actions that may write, delete, deploy, communicate externally, spend money, or use credentials are visible as such.

Built-in tools include the practical low-level operations expected from effective desktop agents: file and text search, file editing, shell execution appropriate to the operating system, and web or other configured information access. A tool may be implemented in Aworkit's Rust code or supplied by an extension.

Node types use a small, versioned extension contract: stable node type and version, declared inputs and outputs, configuration schema, and executor. The contract can evolve without invalidating existing workflow documents. Workflow files store node instances and configuration, never executable plugin code. Unknown data is preserved so a newer workflow remains readable and repairable on an older installation.

## Frozen graph, dynamic execution

The selected workflow graph is frozen when the first input starts the Chat/Run. Editing the saved JSON or canvas afterward affects new Chats only.

Freezing the graph does not make execution static. Conditions, routing, bounded loops, retries, parallel branches, joins, waits, approvals, and evaluators remain dynamic through the graph. An Agent node may plan, select from its configured tools, delegate through allowed nodes, react to results, and revise its approach without rewriting the canvas.

An agent may propose an ordinary change to the saved workflow JSON. Aworkit can show the graph and JSON change for user review and apply it to future Chats after acceptance. The active Chat remains on its frozen workflow. Arbitrary or silent live graph mutation and hidden run-local overlays are not part of the concept.

A possible future **Dynamic Subworkflow** node may create a bounded temporary child graph if a real use case requires it. Such a feature would be explicit and validated; it would not allow unrestricted mutation of the running parent graph.

## Harness Context and execution semantics

The visual workflow is a graphical harness definition, not an input-to-output inference dataflow. The primary thing carried through it is a structured, inspectable **Harness Context**.

The Harness Context can contain:

- the conversation, objective, and current task state;
- system, workflow, user, and generated instruction or prompt layers;
- selected evidence, knowledge, and artifact references;
- working values, observations, and tool results;
- routing decisions and branch lineage;
- loop state and progress; and
- remaining budgets and execution limits.

Prompt and context nodes add, select, remove, summarize, or transform explicit context layers. Model, Agent, Tool, MCP, and External Agent nodes operate on a declared selection of the context and add structured results, observations, artifacts, or decisions.

The Harness Context is not one endlessly growing prompt string. Before a model request, the node compiles the selected and permitted context into the actual provider request. Different nodes can receive different projections. What Aworkit selected and sent remains inspectable.

Context changes through logical revisions. A normal transition revises a context. A branch forks a revision into separate paths. A join explicitly reconciles those paths rather than relying on shared mutable state or last-writer-wins merging.

A workflow may contain several and nested harness loops. A loop has a visible feedback path, exit condition, and limits. A router may choose one branch or several branches. An evaluator may send an unsatisfactory result through another iteration or to another existing path.

**Wait for Input** preserves the current harness position and context and hands control back to the user. The next input resumes the same Chat/Run from that point.

An Agent node can run its own internal model-and-tool cycle. That internal loop is distinct from a harness loop spanning several graph stages. For example, model → tool → model can happen inside an Agent node, while plan → implement → evaluate → revise is a loop visible on the harness canvas.

The three central context operations are therefore:

1. revise a context;
2. fork contexts and reconcile them explicitly; and
3. spawn a temporary child context and integrate its result.

## Temporary subagent contexts

Delegating work does not give a subagent the entire parent context by default. A Subagent node creates a temporary, scoped child context from the parent material selected by the workflow.

The child has its own instructions, conversation, tool activity, working state, loop state, lineage, and limits. It exists until the delegated task completes, fails, is cancelled, or reaches a configured limit. Parallel subagents have separate child contexts rather than shared mutable working state. A child may create another child only when the frozen graph allows nested delegation and the inherited limits permit it.

The subagent returns through a declared result contract. An explicit integration step evaluates, summarizes, transforms, or maps that result into a new parent-context revision. The child's entire conversation and intermediate working state are not merged wholesale into the parent.

When the temporary child context ends, the available provenance remains in the Chat history and inspector. The parent can therefore use a concise, deliberate result without losing the ability to inspect how it was produced.

## Models and portable model tiers

Users configure concrete local and hosted models in the application, including provider-specific parameters such as reasoning effort. Workflows normally reference portable named tiers instead of embedding provider details.

Aworkit reserves four stable tier identifiers in every installation:

| Tier | Selection intent |
|---|---|
| `tier:fast` | Prioritize low response latency among eligible configured models |
| `tier:simple` | Use the least capable model sufficient for straightforward work |
| `tier:balanced` | Use the normal quality, speed, and cost balance |
| `tier:quality` | Maximize expected result quality among eligible configured models |

`fast` and `simple` are deliberately different. A fast model may be relatively expensive but optimized for low latency. A model suitable for simple tasks should be sufficient and inexpensive, but it does not have to be the fastest.

All four standard identifiers always exist. A user maps each one to an exact configured model, an ordered fallback list, or a subordinate model-resolution policy. If only one model is configured, all four may initially map to it. An unmapped standard tier is **Unconfigured**, never missing.

Users can create custom tiers such as `tier:local`, `tier:private`, or `tier:quality-coding`. A custom identifier can genuinely be missing on another installation and is then shown as unresolved. Workflows intended for sharing should normally use the standard identifiers unless they require a specialized target.

Location, privacy, region, tool support, modality, context size, and similar properties are eligibility constraints, not quality-tier names. Cost efficiency applies within every tier rather than requiring a separate `economy` tier, while `local` describes location rather than quality. The Chat history records which concrete provider and model each tier actually resolved to.

## Classification-based harness routing

Routing is a first-class part of the graphical harness. Its main job is not merely to choose a provider after a model tier has already been selected. It classifies the current work and then chooses the appropriate route through the harness.

The core flow is:

```text
current task and Harness Context
        ↓
workflow-defined classifier, often an LLM Router
        ↓
structured classification
        ↓
visible workflow rules
        ↓
graph branch, subagent, external agent, and/or model tier
        ↓
execution and integration back into the harness
```

The workflow defines what information its classifier receives and which dimensions matter. An LLM Router can perform this classification and return the structured result, but the classification and its rules remain explicit parts of the workflow. Useful dimensions include:

- task family, such as explanation, coding, architecture, or research;
- scope, such as local, multi-file, or repository-wide;
- required operations, such as reading, editing, executing, or deploying;
- reasoning depth;
- risk and reversibility;
- ambiguity;
- available verification;
- required context; and
- expected quality.

These are configurable examples, not one fixed global taxonomy. A research harness and a coding harness can classify work differently. Classifier outputs and matching rules are JSON-configurable, and Aworkit supplies useful default router templates for common cases.

Visible rules in the frozen workflow map a classification to destinations already present in the graph. Routing can choose both **who performs the work** and **which model tier is used**. A route may select a normal Agent node, a specialized Subagent, an explicit External Agent node such as Codex, another graph branch, or several branches for parallel evaluation.

An evaluator can visibly loop an unsatisfactory result back for revision or escalate it to a stronger existing path. The classifier cannot invent a node, add a tool, expand authority, or rewrite the frozen workflow.

The inspector records the classification, matched rule, selected path, and result. When a route selects a model tier, a small downstream resolution step maps that tier to the user's configured concrete model or fallback. That is subordinate provider selection, not the main routing architecture. An external agent is selected only through an explicit graph path and is never silently substituted for a model fallback.

## Agents, tools, MCP, and external agents

Aworkit distinguishes models, tools, Aworkit agents, and external agents.

A model produces completions and may participate in an agent loop. An Aworkit Agent node combines a purpose and instructions with selected context, a model target, tools, limits, and a result expected by the harness. A Subagent is a temporary Aworkit agent created for a bounded delegated task.

A tool performs a defined operation. File tools, shells, Python runtimes, web access, MCP servers, and other actions are explicit capabilities in the workflow. Their actual configuration determines what the harness can ask an agent to do.

An external agent owns a richer lifecycle. Codex, for example, has its own model loop, tools, session, progress, and approval behavior. It appears as an explicit **External Agent** node rather than being disguised as an ordinary tool or provider fallback.

Aworkit defines a small normalized external-agent contract and implements specialized adapters where a target exposes richer capabilities. Codex App Server over local standard input/output is the first rich adapter. Agent Client Protocol (ACP) is the next generic path for compatible local coding agents. MCP remains the protocol for tools, resources, and prompts; it is not treated as a universal agent-lifecycle protocol. Agent-to-Agent Protocol (A2A) may later support remote opaque agents, but it is not required for the initial product.

An External Agent node selects a configured target and supplies its task, project scope, budget, and desired result. Its adapter states honestly whether it supports progress, continuation, cancellation, approval requests, or other lifecycle features. Missing capabilities are shown rather than simulated.

Aworkit keeps normalized history and approvals around the delegation and retains the external system's native session identifier where continuation is supported. Selected MCP servers may be passed to an external agent when that adapter and agent support it; Aworkit does not assume all external agents have identical capabilities.

## Trusted extensions and the core boundary

A small trusted Rust core owns essential application responsibilities, while most functional behavior can be extended.

Providers, model integrations, agent profiles and loops, routers, tools, workflow node types, context and memory providers, evaluators, external-agent adapters, and suitable UI additions can be supplied as plugins or services. Essential desktop responsibilities are not ordinary replaceable plugins. The trusted core retains ownership of lifecycle and canonical evidence, workflow authority and approvals, secrets, process supervision, and extension identity, integrity, compatibility, and installed-version records.

Aworkit uses a trusted-extension model. Installing and enabling a third-party extension is an explicit decision to run code with the desktop user's operating-system permissions. Ordinary plugins are not forced into WebAssembly (WASM) and are not placed in mandatory per-plugin sandboxes.

Third-party extensions normally run as subprocesses or local MCP servers. They may use Python, Node.js, Rust, native libraries, GPUs, files, the network, and further subprocesses. The process boundary provides dependency separation, cancellation, restart, logging, crash recovery, and language independence. It is not presented as a security boundary. Suitable bundled components may run inside the application, and a stable native plugin ABI is not required initially.

Each extension has a small JSON manifest describing its identity, version, Aworkit compatibility, entry point, contributed nodes or tools, configuration schema, dependencies, and provenance. The manifest describes the extension; it does not restrict what enabled plugin code can do.

Discovery does not execute an extension. Installation and enabling are separate user actions. Importing a workflow never installs or enables code. Missing plugin nodes remain inert, visible, and losslessly preserved. Aworkit records the extension versions and hashes used by a Chat; an update is treated as new trusted code.

The application warns plainly that an enabled trusted extension may access anything the desktop user can access. Aworkit-mediated tool calls can still follow workflow approvals and scopes, but trusted plugin code may bypass those brokers. Aworkit must not claim otherwise. Users may optionally run a plugin host, worker, or whole project environment in a container, VM, remote machine, or other stronger boundary, but that is not the default plugin contract.

## Workflow authority

For Aworkit-mediated actions, the frozen workflow is the authority contract. An agent may use exactly the tools and external-agent nodes present in that workflow, with their resolved settings and scopes.

Tool names alone are not enough; their modes express real behavior. For example:

- **Project Files** can enforce access to selected project roots.
- **Sandboxed Python** can enforce restricted mounts and network access.
- **Host Python** runs with ordinary unrestricted user-level authority.
- **Host Shell** runs with ordinary unrestricted user-level authority.

A working directory alone is not a restriction. If a tool is described as limited to a project, its implementation must actually enforce the configured roots. Restrictions exist only where the selected tool or execution environment can enforce them.

Before execution, Aworkit shows a concise summary of everything the workflow can access or do, including through tools and external agents—for example, modifying a selected project, using host Python, accessing the web, or delegating to Codex. Imported workflows containing powerful capabilities remain unresolved or inactive until the user binds those capabilities and accepts the resulting authority.

After a workflow has been accepted for a project, the user does not receive a generic approval request for every ordinary action. Further approvals occur where an Approval node or a tool's own configuration requires them.

The running model cannot add tools, replace a restricted tool with an unrestricted variant, expand file roots, add credentials, or otherwise broaden the frozen graph. This guarantee does not cover trusted plugin code, which already runs as trusted user-level software.

An action that may have real effects is not silently retried when Aworkit cannot tell whether the first attempt succeeded. The uncertain outcome is recorded and control returns to the workflow or user.

## Transparency and inspection

Aworkit exposes everything it creates, selects, sends, receives, transforms, stores, or deliberately omits. It cannot expose reasoning or internal behavior that a model provider, plugin, or external agent never supplies. Those visibility limits are stated honestly.

The inspector can show:

- the exact instructions and context selected by Aworkit, including their sources, transformations, token contribution, and redaction;
- routing classifications, considered destinations, matched rules, fallbacks or escalations, and the concrete model or external agent selected;
- model, tool, MCP, subagent, and external-agent activity;
- commands, files, destinations, approvals, artifacts, retries, costs, and verification results; and
- places where a provider, external agent, or trusted extension remained opaque to Aworkit.

Returned model information is labelled using exact semantic categories:

- `reasoning_raw` is reasoning content actually supplied by the source;
- `reasoning_summary` is a summary supplied by the source;
- `progress` is status or intermediate activity; and
- `assistant_output` is the delivered answer or result.

A reasoning summary is never presented as raw reasoning, and Aworkit never invents or reconstructs hidden chain of thought.

Normal history stores semantic events rather than every streaming fragment. Optional detailed capture may retain exact chunks and additional protocol payloads for debugging. That capture has its own retention behavior and is excluded from Git-portable session history by default.

## Configuration, history, and portable sessions

JSON is the sole canonical representation for all Aworkit-owned configuration and workflow definitions. Model, tier, agent, router, tool, plugin, external-agent, and workflow configuration remains transparent and inspectable as JSON. Aworkit does not keep duplicate SQL bodies for these definitions. Secrets are referenced from JSON but stored in the operating-system credential store.

For ordinary local Chats, SQLite stores the canonical semantic Chat/Run events together with machine-oriented operational metadata and indexes. Artifacts remain ordinary files. Process diagnostics use bounded rotating log files.

Semantic events describe what happened. Aworkit rebuilds the Chat view and logical state from those events; it never replays an old model call, shell command, file edit, or external request. This is event-based history, not full application event sourcing and not a replay system for side effects.

### Git-portable project sessions

A project may optionally store a Chat's canonical history inside the project so that it can travel with the source through Git and continue on another computer. This directly addresses the common problem of synchronizing a repository while leaving the related agent session trapped in one computer's application database.

Portable history uses immutable, bounded, parent-linked JSONL segments rather than one endlessly appended file. Permitted large artifacts may be stored by content address. References to project files use project-relative paths. Concurrent continuations from the same history point become explicit Aworkit session branches; timestamps are not used to invent a single order.

The portable record contains enough workflow, event, and provenance information to inspect and continue the Chat, but machine-local capabilities are rebound on the receiving computer. Concrete models, tools, plugins, credentials, and permissions are not assumed to be identical. Missing bindings remain visible and must be resolved before continuation.

Portable history never contains credential values, inherited approvals, or hidden reasoning. Imported records are inert data: loading them does not install plugins, enable code, grant authority, run tools, or repeat prior side effects.

Git portability is opt-in. Aworkit does not automatically commit or push these files. The user decides whether to place them under version control.

Every Chat is stored canonically in either local SQLite or the project-portable JSONL stream, never both. A local SQLite index may accelerate inspection and search over portable history, but that index is disposable.

## Management Chat and recurring-error repair

The pinned Management Chat is the primary conversational control surface for configuring models and integrations, creating and editing workflows, inspecting other Chats, understanding routing behavior, and maintaining Aworkit. Its management workflow defaults to `tier:quality` and may use other tiers or delegate work to configured external agents.

Aworkit maintains a persistent recurring-error ledger. It groups repeated tool, plugin, integration, and runtime failures so future Chats do not rediscover the same problem and workaround from scratch. Management Chat can see earlier occurrences, known diagnoses, fixes or workarounds, verification results, and whether a previously repaired failure has returned as a regression.

Maintenance and repair are explicit, bounded tasks, not continuous background self-improvement. A repair begins because the user asks for it or chooses **Investigate and fix**. Within that bounded task, the management workflow may investigate the error, modify configuration, workflows, plugins, or Aworkit's own source, compile, test, benchmark, prepare a candidate, restart the application, resume the same management task, and verify the result after restart without asking for approval at every intermediate step.

Before a candidate can become active, Management Chat presents:

- its diagnosis and proposed solution;
- the complete source and configuration diff;
- tests and benchmarks performed;
- expected behavioral consequences and unresolved uncertainty;
- anything important removed, disabled, broadened, or replaced; and
- the rollback point.

The user then decides whether to select **Activate and restart**. This confirmation is the decisive activation gate. If the candidate removes a sandbox or applies some other broad workaround, that fact must be plainly disclosed, but Aworkit does not need a complex mechanism that tries to decide whether the workaround is acceptable. Reviewing that choice is the user's responsibility.

The management task is checkpointed before restart. A previous working build remains available. If the candidate fails to start or fails the focused post-restart verification, Aworkit returns to the previous working build and reports the rollback. The error ledger records the outcome; a later recurrence reopens the issue as a regression instead of launching another repair automatically.

Changing and rebuilding Aworkit requires a configured source checkout and the necessary toolchain. If those are unavailable, Management Chat may diagnose the problem and prepare a configuration change, patch, workaround, or issue report, but it cannot claim to have compiled or verified a new application build.

Open-ended loops that repeatedly rewrite and promote the application, automatic activation of code repairs, and long-running autonomous self-improvement are outside this concept.

## Technical architecture and Rig boundary

Aworkit uses Rust and Tauri in a multi-process desktop architecture:

- Tauri provides the presentation layer.
- A small trusted Rust core owns the application lifecycle and essential contracts.
- Agent and workflow workers execute Chat/Run harnesses.
- Shells, MCP servers, plugin hosts, Codex, and other native integrations run as supervised sidecars where appropriate.

Process boundaries support crash isolation, cancellation, restart, resource management, dependency separation, and reliable cleanup of complete process trees. A worker failure must not take down the desktop UI or corrupt committed Chat history. A subprocess is not called a security sandbox unless a real sandbox, container, VM, or remote boundary enforces that claim.

Isolation is risk-adaptive rather than mandatory for every Chat. A user can select a stricter sandboxed, containerized, virtualized, or remote workspace for work that needs it without making that the plugin model or the normal execution requirement.

The trusted core owns canonical Chat lifecycle and evidence, frozen workflow authority and approvals, secret brokerage, process supervision, and extension identity and integrity. Functional behavior remains broadly extensible through versioned Aworkit contracts.

Rig supplies useful Rust primitives for provider-neutral completion, streaming, tool-call conversion, MCP helpers, and agent loops. Aworkit owns workflows, model targets, classification routing, authority, approvals, retries, normalized events, usage, persistence, history, recovery, and desktop behavior.

A narrow Aworkit-owned boundary translates stable request, result, event, usage, and error types to the selected Rig version. Workflow JSON, configuration JSON, database records, plugin contracts, and canonical history contain Aworkit types rather than Rig-native types.

`rig-core` may be used broadly for supported provider calls. `rig-agent` may implement suitable Agent nodes, but it is one replaceable loop implementation rather than the workflow runtime or the only possible harness. Codex, Claude, and other external agents keep separate lifecycle adapters instead of being forced through Rig.

Rig snapshots and internal AgentRun state are never canonical Aworkit persistence. Rig can be upgraded, replaced, or bypassed for a provider or agent loop without changing workflow files or migrating Aworkit session history.
