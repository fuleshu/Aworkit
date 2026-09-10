# DeepSeek Harness → Aworkit tool and plugin comparison

Reviewed 9 September 2026 against the local working trees: DeepSeek Harness `b150a551b8`, Aworkit `15e2ac2`. This is a source/registry investigation, not a test of the running installed applications. No application code or configuration was changed for this report.

Your pasted inventory has **166 instances, 139 distinct plugin names, 137 enabled instances and 29 disabled instances**. Most entries are services, policies or UI components rather than tools the model can call. Repeated names are consolidated below, with the original instance counts preserved.

Aworkit's native manifest currently contains **15 callable tools plus Workspace Instructions as automatic context**. MCP adds the tools exposed by configured servers. A tool present in the registry still needs to be enabled/selected for a new Chat; existing Chats retain their frozen bindings.

Priority is a recommendation for the **missing capability**: **1 = first implementation wave**, **2 = next wave**, **3 = optional/later**, **0 = no useful direct adoption in Aworkit**. Blank cells mean a functional counterpart exists. “Partial” means the named Aworkit capability exists, but the third column identifies a material gap; its priority applies only to that gap. A functional counterpart does not claim identical schemas, limits, permissions, storage format or lifecycle. Aworkit's native file tools accept absolute paths and use approval for external locations.

## Model-callable tools

The first table expands tool plugins into their actual callable names. Parent tools, child-only tools, conditional functions and disabled optional instances are marked explicitly.

| DeepSeek tool / plugin | Matching Aworkit tool / feature | Missing capability: description and relevance to Aworkit | Priority |
|---|---|---|---|
| `read` · tool-fs | Project file read · `read_file` |  |  |
| `write` · tool-fs | Project file write · `write_file` |  |  |
| `edit` · tool-fs | Project file edit · `edit_file` |  |  |
| `read_image` · tool-fs | —; user image attachments already exist | Lets the agent open a local image and send it to its vision model. Highly relevant for screenshots, diagrams, and UI verification; user-uploaded images do not cover this invocation path. | 1 |
| `glob` · tool-fs-search | Project file list (glob) · `list_files` |  |  |
| `grep` · tool-fs-search | Project file regex search · `grep_files` |  |  |
| `pwsh` · tool-pwsh | Partial: Host shell · `shell` (configure PowerShell) | Foreground execution is covered. Missing managed background execution and its job handle; relevant for long builds and commands, alongside the job tools. | 1 |
| `job_list` · tool-jobs | — | Lists owned background shell/subagent jobs. Useful for long builds and parallel tasks; implement with the background-job runtime. | 1 |
| `job_output` · tool-jobs | — | Reads or waits for background-job output/status. Essential once commands can continue beyond a foreground tool call. | 1 |
| `job_kill` · tool-jobs | —; whole-Run Stop is a different operation | Stops one background job and its process/agent work. Needed alongside job creation and output retrieval. | 1 |
| `skill` · tool-skill | Skills · `skill` (`tool.skill`) |  |  |
| `todo_write` · tool-todo | Run task list · `todo` |  |  |
| `web_search` · tool-web | Web search · `web_search` |  |  |
| `ask_user_question` · tool-ask-user | —; approval cards cover authorization only | Asks structured questions and waits for the human's answer within a run. Highly relevant for requirements and user-owned choices. | 1 |
| `exit_plan_mode` · plan-mode | —; Aworkit has a Plan workflow node | Submits a complete plan for review and exits planning after approval. Relevant; a structured Plan node does not provide this interactive mode/lifecycle. | 2 |
| `create_goal` · tool-goal | —; a todo list is related | Creates a durable objective, optionally with a budget, for continued execution. Useful for user-authorized work across multiple rounds. | 2 |
| `get_goal` · tool-goal | — | Reads goal status and budget/usage. Implement together with goal creation and the round driver. | 2 |
| `update_goal` · tool-goal | — | Changes goal state under the caller's allowed authority, including completion/blocking and authorized lifecycle edits. Useful with the complete goal subsystem. | 2 |
| `subagent` · tool-subagent, spawn instance | Partial: Subagent delegation · `spawn_subagent` | Aworkit runs a fresh foreground child with a restricted read-only tool set and no further delegation. Missing: continuable/background children and broader explicitly granted child capabilities. | 1 |
| `subagent_fork` · tool-subagent, fork instance | —; UI Chat Fork exists | Starts a child with a snapshot of the parent's conversation. Useful when copying all necessary context into a fresh subtask is difficult; UI Chat Fork is not an agent-callable child fork. | 2 |
| `list_agents` · tool-subagent-control/list-agents | — | Lists live/known child agents and their state. Relevant for supervising concurrent delegation. | 1 |
| `send_message` · tool-subagent-control | — | Sends guidance to an existing continuable child. Relevant for corrections and sharing findings without starting over. | 1 |
| `interrupt_agent` · tool-subagent-control | — | Interrupts a selected continuable child's current work. Relevant for stopping obsolete work or redirecting a child. | 1 |
| `report` · tool-subagent-report; child-only | —; a final subagent result is related | Lets a live continuable child report to its direct parent before its final completion. Relevant once background children and messaging exist. | 2 |
| `workflow` · tool-workflow | Partial: visual workflow editor + Rust workflow worker | Runs an agent-authored JavaScript orchestration script with child agents, structured results, and phases. Aworkit's preconfigured workflow graphs are related; no equivalent model-callable script tool was found. | 2 |
| `ralph` · tool-ralph | — | Runs successive fresh agents toward one fixed objective, carrying bounded handoffs and shared workspace state. Useful for explicit iterative coding requests after writable child/workflow support; specialized rather than a general agent-loop replacement. | 3 |
| `run_code` · tools + code-runtime-worker-thread; conditional | —; Host Python executes scripts only | Runs code that calls the registered tools through a generated SDK and their normal approval/logging pipeline. Relevant for batching and reducing model/tool round trips; ordinary Python or shell execution is not equivalent. | 2 |
| `web_fetch` · tool-web; disabled by checked-in standard preset | Web page fetch · `web_fetch`; also multi-page `web_extract` |  |  |
| `bash` · tool-bash; disabled in pasted Windows list | Host shell · `shell` (when a Bash/POSIX executable is configured) |  |  |
| `str_replace_editor` · tool-str-replace-editor; disabled | Functional overlap: Project file read/write/edit |  |  |
| `subagent_codex` · optional disabled tool-subagent instance | —; external-agent/Codex adapter infrastructure is related | Delegates through an external Codex provider. Relevant if Aworkit should delegate to a separate coding agent, but no matching callable native tool is registered. | 3 |
| `subagent_claude_code` · optional disabled tool-subagent instance | — | Delegates through an external Claude Code provider. An optional integration for users of that agent; no matching callable native tool is registered. | 3 |
| Server-defined tools · mcp-client | Configured MCP tools · frozen server/function bindings |  |  |

**Configuration caveats:** the checked-in standard preset enables `tool-web` with `fetch: false`, so only `web_search` is registered there. `run_code` is conditional on `tools.mode=code|both`; the checked-in web configuration takes `DSH_TOOLS_MODE` and otherwise defaults to native tools. An enabled code runtime does not prove code mode is active. `read_image` requires attachments and a vision-capable routed model at execution. `report` is available to continuable children, not the root agent. The standard preset configures both `subagent` and `subagent_fork` as continuable; the generated catalog's generic alias note describes a different/default composition, so the actual preset source takes precedence. User/profile overrides may alter these settings.

The two enabled `tool-subagent` instances correspond to spawn and fork in the checked-in standard preset. Its optional Codex/Claude Code aliases are explicitly disabled. The disabled base-layer copies are not extra active tools. Exact server-defined MCP tool names cannot be recovered from the label `mcp-client`; they depend on server discovery and configuration.

Evidence: [Harness generated tool catalog](C:/src/deepseek-harness/docs/tool-catalog.md), [standard agent preset](C:/src/deepseek-harness/apps/cli/config/agent-presets/standard/agent.cordis.yml), [web bundle configuration](C:/src/deepseek-harness/packages/bundle/web-app/cordis.patch.yml), [Aworkit native registry](C:/src/Aworkit/desktop/tool-plugins/aworkit-native/tool-plugin.json), [native tool dispatch and child limits](C:/src/Aworkit/desktop/src-tauri/src/runtime/tool_loop.rs:150), [Aworkit Plan-node contract](C:/src/Aworkit/desktop/src-tauri/src/runtime/plan_contract.rs), [Chat image input](C:/src/Aworkit/docs/chat-images.md).

## Complete pasted plugin inventory

This table covers **every one of the 139 distinct names** in your list, in first-appearance order. The status counts in column one reproduce your pasted list; they are not a fresh runtime inventory. For non-tool plugins, column two names the matching Aworkit feature or infrastructure where there is one. Multiple rows can belong to one implementation effort; priorities are not separate requests to implement each plugin.

| DeepSeek tool / plugin | Matching Aworkit tool / feature | Missing capability: description and relevance to Aworkit | Priority |
|---|---|---|---|
| `include` · 1 enabled | — | Cordis configuration include/composition machinery. Not an agent tool; transplanting it is not useful in Aworkit's Rust/manifest architecture. | 0 |
| `timer` · 1 enabled | Native Rust/JavaScript timers and runtime deadlines |  |  |
| `hmr` · 2 enabled, 1 disabled | Vite/Tauri development hot reload; development counterpart |  |  |
| `llm` · 1 enabled | Capability-host model/provider abstraction |  |  |
| `session` · 1 enabled | Durable Chat/Run history and semantic events |  |  |
| `typert-registry` · 1 enabled | — | Typert's TypeScript type/RPC metadata infrastructure. Aworkit uses Rust protocol types and Tauri/IPC; a direct port has no useful role. | 0 |
| `typert-loader` · 1 enabled | — | Typert's TypeScript type/RPC metadata infrastructure. Aworkit uses Rust protocol types and Tauri/IPC; a direct port has no useful role. | 0 |
| `api-gateway` · 1 enabled | Trusted-core / capability-host gateways and desktop commands; architectural counterpart |  |  |
| `session-title` · 1 enabled | Chat title projection from the initial user input |  |  |
| `session-title-first-prompt-llm` · 1 enabled | Partial: deterministic first-input title | Uses an LLM to produce a short conversation title. Optional usability improvement; Aworkit currently compacts the input text directly. | 3 |
| `user-questions` · 1 enabled | —; tool approvals are separate | Structured human question/answer service used during active runs. Relevant; add with ask_user_question and its UI. | 1 |
| `agent` · 1 enabled | Agent workflow node and native model/tool loop |  |  |
| `agent-default-model` · 1 enabled | Provider/model tiers resolved and frozen for each Chat |  |  |
| `jobs-local` · 1 enabled | — | Owned background-job registry and lifecycle. Relevant foundation for long commands, background children, output retrieval, cancellation, and completion notices. | 1 |
| `llm-retry` · 1 enabled | Partial: native provider timeout/overflow recovery | A configurable provider-routed retry policy is broader than the inspected recovery paths. Relevant for transient failures; review existing recovery before extending it. | 2 |
| `settings-file` · 1 enabled | Persisted Settings V2 documents |  |  |
| `credentials-local` · 1 enabled | Credential references and platform credential store |  |  |
| `llm-pi-ai` · 1 enabled | DeepSeek via Aworkit's OpenAI-compatible adapter; functional counterpart |  |  |
| `session-persistence-jsonl` · 1 enabled | SQLite-backed LocalHistoryStore; persistence counterpart, different format |  |  |
| `attachment-local` · 1 enabled | Validated local image-blob storage and durable attachment references |  |  |
| `session-query-sqlite` · 1 enabled | Partial: Chat history index; no comparable full-text session search UI found | Searches session content using SQLite FTS5. Useful for recovering earlier work. The checked-in web preset sets openAt: never, so Enabled alone does not imply content indexing is active. | 2 |
| `session-projection` · 1 enabled | History and run-event projections |  |  |
| `session-telemetry-otel` · 1 enabled | —; local diagnostic/run events exist | Exports session telemetry through OpenTelemetry. Useful for optional diagnostics and performance analysis; not required for local agent execution. | 3 |
| `subprocess-local` · 1 enabled | aworkit-process and capability-host process execution |  |  |
| `sandbox-local` · 1 enabled | Partial: isolation contracts and host execution limits | Provides concrete OS process sandbox backends, including Windows restricted-token/ACL confinement. Aworkit's registered Host shell/Python tools use host execution; equivalent active OS confinement was not found on those tool paths. | 1 |
| `sandbox-policy` · 1 enabled | Partial: frozen tool authority, project boundaries, approval modes | Resolves a shared per-call sandbox policy for filesystem and shell operations. Relevant with real sandboxed shell execution; approval mode alone does not provide confinement. | 1 |
| `bash-sandbox` · 1 disabled | —; Host shell is related | Runs Bash under the selected OS sandbox. Relevant for Bash-capable deployments after adding real sandbox execution; disabled in the pasted Windows list. | 2 |
| `pwsh-sandbox` · 1 enabled | —; Host shell can invoke PowerShell | Runs PowerShell through the sandbox policy/backend. Relevant on Windows; the ordinary Host shell path is not an equivalent sandbox. | 1 |
| `user-approval` · 1 enabled | Native approval broker, approval cards, durable decisions |  |  |
| `permission-presets` · 1 enabled | Partial: Chat approval-mode selector | Bundles sandbox mode and approval policy in one preset. Relevant after sandbox modes exist; Aworkit's approval selector covers only part of that choice. | 2 |
| `shell-env` · 1 enabled | Frozen shell executable, shell syntax context, child environment construction |  |  |
| `tool-bash` · 2 disabled | Host shell · shell; Bash when configured (listed disabled) |  |  |
| `tool-pwsh` · 1 enabled, 1 disabled | Partial: Host shell · shell | Foreground PowerShell execution is covered. Missing DeepSeek's managed run_in_background option and job registration; implement with jobs-local/tool-jobs. | 1 |
| `tool-jobs` · 1 enabled, 1 disabled | — | Registers job_list, job_output, job_kill. Relevant with the owned background-job runtime; these are three tools from one plugin. | 1 |
| `fs-observation-policy` · 1 enabled | Partial: exact-match edits and atomic content-hash checks | Tracks what the agent has read and rejects edits/writes against unobserved or changed versions. Relevant: Aworkit's edit hashes its execution-time read, while its write path passes no expected content hash; this is not the same model-observation guard. | 1 |
| `tool-fs` · 1 enabled, 1 disabled | Native read/write/edit and image-read tools | Text and image tools accept local paths with location-based approval; shell sandboxing remains separate. | 1 |
| `tool-fs-search` · 1 enabled, 1 disabled | Project file list (glob) and Project file regex search |  |  |
| `agent-instructions` · 1 enabled, 1 disabled | Workspace Instructions · tool.workspace_instructions (automatic context) |  |  |
| `skill` · 1 enabled | Native skill discovery/catalog and loader services |  |  |
| `skill-filesystem` · 1 enabled, 1 disabled | Filesystem skill provider: .aworkit/skills, .agents/skills, custom roots |  |  |
| `skill-badge` · 1 disabled | — | Supplies the DeepSeek-specific powered-by-dsh badge skill and asset. Not useful as an Aworkit feature; disabled in the pasted list. | 0 |
| `tool-skill` · 1 enabled, 1 disabled | Skills · skill (tool.skill) |  |  |
| `commands` · 1 enabled | Partial: direct /skill invocation and explicit UI actions | General plugin-owned human slash-command registry. Relevant if Aworkit adds discoverable /goal, /plan, /export commands; current internal app commands are not this composer API. | 3 |
| `command-feedback` · 1 enabled | — | Captures /feedback text as a session event without a model turn. Optional product-feedback feature. | 3 |
| `goal` · 1 enabled | — | Durable goals, authorized automatic continuation rounds, and the human /goal command. Relevant for sustained work; implement as one subsystem with goal tools and UI. | 2 |
| `goal-round-driver` · 1 enabled | — | Durable goals, authorized automatic continuation rounds, and the human /goal command. Relevant for sustained work; implement as one subsystem with goal tools and UI. | 2 |
| `command-goal` · 1 enabled | — | Durable goals, authorized automatic continuation rounds, and the human /goal command. Relevant for sustained work; implement as one subsystem with goal tools and UI. | 2 |
| `plan-mode` · 1 enabled, 1 disabled | Partial: structured Plan workflow node | Interactive planning state and reviewed exit_plan_mode transition are missing. Useful for a user-controlled plan-then-implement workflow. | 2 |
| `token-meter` · 1 enabled | Context usage estimates, provider token usage, compaction pressure measurement |  |  |
| `compaction-basic` · 1 enabled, 1 disabled | Native automatic context compaction |  |  |
| `command-compact` · 1 enabled, 1 disabled | Context details → Compact context; UI counterpart to /compact |  |  |
| `subagent` · 1 enabled | Partial: foreground spawn_subagent | Fresh read-only delegation exists. Missing continuable/background children and richer granted child capabilities; relevant for independent coding work. | 1 |
| `subagent-spawn-in-process` · 1 enabled | Partial: foreground spawn_subagent | Fresh read-only delegation exists. Missing continuable/background children and richer granted child capabilities; relevant for independent coding work. | 1 |
| `subagent-fork-in-process` · 1 enabled | —; UI Chat Fork is related | Forks a child agent from the parent's conversation snapshot. Relevant for delegated work needing the existing context; UI Chat Fork does not provide this tool lifecycle. | 2 |
| `tool-subagent-control` · 1 enabled, 1 disabled | — | Registers send_message and interrupt_agent for continuable children. Relevant with background delegation. | 1 |
| `tool-subagent-control/list-agents` · 1 enabled, 1 disabled | — | Registers list_agents with child state/identity. Relevant for supervising concurrent agents. | 1 |
| `tool-subagent` · 2 enabled, 4 disabled | Partial: spawn_subagent | Configured instances expose subagent and subagent_fork; optional Codex/Claude Code instances are disabled. Aworkit lacks the continuable/fork/control lifecycle; see the callable-tool table for individual priorities. | 1 |
| `tool-subagent-report` · 1 enabled | —; final child result is related | Registers report only inside a continuable child so it can update its direct parent while working. Useful after messaging/background children exist. | 2 |
| `workflow-worker-thread` · 1 enabled, 1 disabled | Partial: Rust workflow worker and visual graph editor | Runs model-authored JavaScript workflows that call children and collect structured results. Aworkit's saved graph execution is related; no equivalent callable script engine is registered. | 2 |
| `tool-workflow` · 1 enabled, 1 disabled | Partial: Rust workflow worker and visual graph editor | Runs model-authored JavaScript workflows that call children and collect structured results. Aworkit's saved graph execution is related; no equivalent callable script engine is registered. | 2 |
| `tool-call-timeout-policy` · 1 enabled | Frozen invocation limits, cancellation and process/model deadlines |  |  |
| `spill-local` · 1 enabled | Immutable result archives, bounded previews, and Context retrieval · context; Aworkit reference mechanism differs |  |  |
| `spill-policy` · 1 enabled | Immutable result archives, bounded previews, and Context retrieval · context; Aworkit reference mechanism differs |  |  |
| `session-checkpoint-policy` · 1 enabled | Durable broker admission/settlement and history/context checkpoints |  |  |
| `compaction-tool-result-pruner` · 1 enabled, 1 disabled | Integrated tool-result pruning before context compaction |  |  |
| `tool-todo` · 1 enabled, 1 disabled | Run task list · todo |  |  |
| `tool-goal` · 1 enabled, 1 disabled | — | Registers create_goal, get_goal, update_goal. Relevant with durable objective state and authorized continuation. | 2 |
| `tool-ralph` · 1 enabled, 1 disabled | — | Fresh-agent iterative execution with bounded handoffs. Specialized; useful after child/workflow support, for explicit Ralph-style requests. | 3 |
| `tool-str-replace-editor` · 1 disabled | Project read/write/edit tools cover the central operations; alternate tool is listed disabled |  |  |
| `repeat-tool-reminder` · 1 enabled | Native repeated-tool reminders |  |  |
| `web` · 1 enabled | Capability-host WebTools and pluggable web-search backends |  |  |
| `web-search-deepseek` · 1 enabled | DeepSeek web-search backend |  |  |
| `tool-web` · 1 enabled, 1 disabled | Web search/fetch/extract; checked-in DeepSeek standard preset disables its fetch function |  |  |
| `tools` · 1 enabled | Partial: native registry, provider schemas and dispatch | The registry role is covered. Missing optional code/both mode with run_code and generated tool SDK; useful for batching and orchestration. | 2 |
| `system-prompt` · 1 enabled | Frozen persona and selected-tool/workspace/skill context assembly |  |  |
| `agent-loop` · 1 enabled | Agent workflow node and native model/tool loop |  |  |
| `fs-sandbox` · 1 enabled | Anchored directory capabilities and frozen per-invocation file locations with external-path approval; functional counterpart |  |  |
| `llm-deepseek` · 1 enabled | OpenAI-compatible model adapter supporting DeepSeek |  |  |
| `code-runtime-worker-thread` · 1 enabled | —; Host Python is related | JavaScript execution behind run_code, with controlled calls into the registered tool pipeline. Useful for code-based tool orchestration; Host Python is not a tool bridge. | 2 |
| `storage` · 1 enabled | Document repositories, SQLite history and validated settings/domain documents; different storage architecture |  |  |
| `storage-json` · 1 enabled | Document repositories, SQLite history and validated settings/domain documents; different storage architecture |  |  |
| `storage-domain` · 1 enabled | Document repositories, SQLite history and validated settings/domain documents; different storage architecture |  |  |
| `message-feedback` · 1 enabled | — | Per-message rating and note storage. Optional for user feedback and evaluation. | 3 |
| `session-log-export` · 1 enabled | —; durable history exists | Exports a conversation log through a download dialog and /export command. Useful for sharing diagnostics and archiving work; no equivalent user-facing export flow was found. | 2 |
| `workspace` · 1 enabled | Saved Projects and frozen Chat workspace bindings |  |  |
| `session-projection-cache` · 1 enabled | History index and persisted/rebuildable runtime projections; architectural counterpart |  |  |
| `session-reference` · 1 enabled | —; Chat Fork is related | Adds bounded snapshots of another session as explicit untrusted context. Useful for continuing or combining earlier work without forking it. | 2 |
| `file-reference-local` · 1 enabled | —; project file tools are available | Fuzzy file-reference discovery and @file resolution. Useful for attaching exact project files through the composer. | 2 |
| `session-stats` · 1 enabled | Partial: context usage, Run details and event timings | Dedicated whole-conversation counts and wall-time summary are broader than the existing usage display. Optional observability improvement. | 3 |
| `directory-picker-auto` · 1 enabled | Native project directory picker |  |  |
| `plugin-inventory` · 1 enabled | Settings → Tools plugin inventory and MCP catalog; different plugin scope |  |  |
| `apiproxy` · 1 enabled | Tauri command/event bridge and trusted-core IPC; architectural counterpart |  |  |
| `cordis-host-runner` · 1 enabled | — | Live Cordis package definitions and host/browser execution/inspection. Harness-stack-specific self-extension; no direct port is useful for Aworkit's Rust/Tauri runtime. | 0 |
| `web-app/startup` · 1 enabled | Tauri desktop startup and WebView application; delivery counterpart |  |  |
| `webserver` · 1 enabled | Tauri desktop startup and WebView application; delivery counterpart |  |  |
| `web-app` · 1 enabled | Tauri desktop startup and WebView application; delivery counterpart |  |  |
| `modules` · 1 enabled | Compiled frontend modules and Tauri runtime; architectural counterpart |  |  |
| `connection` · 1 enabled | Tauri command/event bridge and trusted-core IPC; architectural counterpart |  |  |
| `api-remotes` · 1 enabled | Tauri command/event bridge and trusted-core IPC; architectural counterpart |  |  |
| `runtime` · 1 enabled | Compiled frontend modules and Tauri runtime; architectural counterpart |  |  |
| `cordis-client-runner` · 1 enabled | — | Live Cordis package definitions and host/browser execution/inspection. Harness-stack-specific self-extension; no direct port is useful for Aworkit's Rust/Tauri runtime. | 0 |
| `ui-theme` · 1 enabled | Appearance Settings: system/light/dark and font scaling |  |  |
| `locale` · 1 enabled | — | Localized UI strings and language preference. Relevant for a multilingual product; not required for tool capability parity. | 3 |
| `ui-layout` · 1 enabled | React desktop shell and component rendering |  |  |
| `ui-renderer` · 1 enabled | React desktop shell and component rendering |  |  |
| `ui-sidebar` · 1 enabled | NavigationPane: Projects, Chats, history and pins |  |  |
| `ui-settings` · 1 enabled | SettingsScreen and native settings persistence |  |  |
| `ui-settings-general` · 1 enabled | SettingsScreen and native settings persistence |  |  |
| `ui-settings-models` · 1 enabled | Provider/model settings and discovery |  |  |
| `ui-settings-plugin-inventory` · 1 enabled | Settings → Tools plugin inventory and MCP catalog; different plugin scope |  |  |
| `ui-conversation` · 1 enabled | ChatWorkspaceScreen and ConversationTimeline |  |  |
| `ui-brand-official` · 1 enabled | Aworkit's own product branding |  |  |
| `ui-attachment` · 1 enabled | Composer image attachment thumbnails and previews |  |  |
| `ui-tool` · 1 enabled | Conversation tool/activity cards and Run details |  |  |
| `ui-cordis` · 1 enabled | — | Live Cordis package definitions and host/browser execution/inspection. Harness-stack-specific self-extension; no direct port is useful for Aworkit's Rust/Tauri runtime. | 0 |
| `ui-workflow-run` · 1 enabled | Workflow graph execution and Run details |  |  |
| `ui-deliverables` · 1 enabled | —; normal assistant content and tool results are related | Recognizes final-response file references and presents deliverables for opening/downloading. Relevant for documents, images and generated project outputs. | 2 |
| `ui-workspace` · 1 enabled | Project picker and project-scoped Chats |  |  |
| `ui-input-trigger` · 1 enabled | —; direct /skill text handling exists | Reusable composer trigger/autocomplete menus. Useful for discovering skills, commands and file references. | 2 |
| `ui-commands` · 1 enabled | —; ordinary app controls and /skill text handling exist | Composer UI for the human slash-command registry. Optional convenience after the underlying command features exist. | 3 |
| `ui-skill` · 1 enabled | Partial: Skills Settings, catalog injection and invocation | Dedicated skill selection/reference UI is missing from the inspected composer. Useful for discovering and inserting available skills. | 2 |
| `ui-subagent` · 1 enabled | Partial: subagent activity/result cards | Missing an interactive view of continuable/background agents and their controls. Relevant with richer subagent lifecycle support. | 2 |
| `ui-reference` · 1 enabled | — | Composer/display UI for file and session references. Useful with file-reference-local and session-reference. | 2 |
| `ui-jobs` · 1 enabled | — | Shows background jobs, their outputs and stop controls. Relevant with the job runtime and tools. | 1 |
| `ui-goal` · 1 enabled | — | Shows active objective, progress, budgets and lifecycle controls. Relevant with the goal subsystem. | 2 |
| `ui-message-feedback` · 1 enabled | — | Per-message rating/note controls. Optional product-feedback feature. | 3 |
| `ui-model-selection` · 1 enabled | Workflow/model tier selection before Chat configuration freezes |  |  |
| `ui-permission-presets` · 1 enabled | Partial: Chat approval selector | Combined sandbox/approval presets are missing; pair with a real sandbox policy, rather than relabeling approval modes. | 2 |
| `ui-agent-preset` · 1 enabled | Workflow templates and Chat workflow selection; functional counterpart |  |  |
| `ui-settings-plugins` · 1 enabled | Settings → Tools and MCP configuration forms |  |  |
| `ui-plan` · 1 enabled | Partial: Plan output and workflow Approval nodes | Interactive plan-mode review UI is missing. Relevant with exit_plan_mode and its durable transition. | 2 |
| `ui-user-questions` · 1 enabled | — | Structured question cards and human responses during a run. Add alongside ask_user_question. | 1 |
| `ui-trajectory` · 1 enabled | Partial: ConversationTimeline, Run details and raw Context panel | Dedicated searchable/table/timeline trajectory inspection is broader than existing views. Useful for debugging long runs; lower priority than execution features. | 3 |
| `agent-presets` · 1 enabled | Workflow templates and Chat workflow selection; functional counterpart |  |  |
| `persona` · 1 enabled | Editable default persona and frozen Agent prompt |  |  |
| `tool-ask-user` · 1 enabled | — | Registers ask_user_question. Relevant for structured clarification and human choices during execution. | 1 |
| `mcp-client` · 1 enabled | MCP configuration, discovery, frozen selection and native stdio/HTTP dispatch |  |  |
| `directory-picker-native` · 1 enabled | Native project directory picker |  |  |
| `ui-directory-picker-native` · 1 enabled | Native project directory picker |  |  |

## Recommended implementation order

1. Structured human questions and local-image reading; safe observed-version file writes; background-job ownership, output and cancellation. Add a real Windows sandbox backend and policy integration for shell/Python execution.
2. Extend the existing subagent implementation with background/continuable children and control tools; add the corresponding job/agent UI. Then implement fork/report support where useful.
3. Add interactive plan approval, durable goals and authorized continuation, and a model-callable workflow/code orchestration surface. Implement each as a complete lifecycle with its UI and persistence, not just a tool schema.
4. Add file/session references, deliverable handling, history export and search. Keep Ralph loops, external-agent aliases, ratings, telemetry and localization for later unless they are explicit product priorities.

This sequence is my assessment of Aworkit's current gaps, not a DeepSeek requirement. No priority is assigned to rebuilding capabilities that already have counterparts, such as skills, workspace instructions, compaction, web search/fetch, todo lists or basic file tools.

## Supporting Aworkit source evidence

- [Native registry](C:/src/Aworkit/desktop/tool-plugins/aworkit-native/tool-plugin.json) and [runtime tool registry](C:/src/Aworkit/desktop/src-tauri/src/runtime/tool_registry.rs): callable tool identities, schemas, settings, automatic-context distinction.
- [Native tool execution](C:/src/Aworkit/desktop/src-tauri/src/runtime/tool_loop.rs:2992): foreground fresh-child execution and the restricted child tool set. [File execution](C:/src/Aworkit/desktop/src-tauri/src/runtime/tool_loop.rs:2590) shows execution-time exact matching and the write path's absent expected hash; [filesystem implementation](C:/src/Aworkit/crates/aworkit-capability-host/src/files.rs) supplies atomic hash checks.
- [Host process tools](C:/src/Aworkit/crates/aworkit-capability-host/src/tools.rs), [shell dialect/environment](C:/src/Aworkit/crates/aworkit-capability-host/src/shell.rs), [isolation architecture](C:/src/Aworkit/crates/aworkit-capability-host/src/isolation/mod.rs) and [approval behavior](C:/src/Aworkit/docs/approvals.md): host execution, optional isolation infrastructure and actual tool approval semantics are distinct.
- [Skills implementation](C:/src/Aworkit/crates/aworkit-capability-host/src/skills/mod.rs), [runtime skill integration](C:/src/Aworkit/desktop/src-tauri/src/runtime/tool_loop/skills.rs) and [Workspace Instructions](C:/src/Aworkit/docs/workspace-instructions.md): both skill loading and automatic instructions exist. Older “not implemented” text in historical notes/doc subsections is not used as current evidence.
- [Context compaction](C:/src/Aworkit/docs/context-compaction.md), [compression and retrieval](C:/src/Aworkit/docs/context-compression.md), [repeated-tool reminders](C:/src/Aworkit/desktop/src-tauri/src/runtime/repeat_tool_reminder.rs): these are existing runtime capabilities.
- [History/index](C:/src/Aworkit/desktop/src-tauri/src/runtime/history_index.rs), [title generation](C:/src/Aworkit/desktop/src-tauri/src/runtime/history.rs:2107), [Chat composer](C:/src/Aworkit/desktop/src/chat/ChatComposer.tsx), [ConversationTimeline](C:/src/Aworkit/desktop/src/chat/ConversationTimeline.tsx), [Run details](C:/src/Aworkit/desktop/src/chat/runDetails.ts), [Context panel](C:/src/Aworkit/desktop/src/chat/ContextUsage.tsx) and [navigation](C:/src/Aworkit/desktop/src/shell/NavigationPane.tsx): current user-facing comparison points.
- [Settings](C:/src/Aworkit/desktop/src/workbench/SettingsScreen.tsx), [plugin library](C:/src/Aworkit/desktop/src/workbench/settings-v2/ToolPluginLibrary.tsx), [MCP setup](C:/src/Aworkit/desktop/src/workbench/settings-v2/McpServerSetup.tsx) and [MCP runtime](C:/src/Aworkit/desktop/src-tauri/src/runtime/mcp_tools.rs): plugin inventory/configuration and dynamic tools.
- [Rust workflow worker](C:/src/Aworkit/crates/aworkit-workflow-worker/src/runtime.rs) and [graph execution](C:/src/Aworkit/desktop/src-tauri/src/runtime/graph_pass.rs): configured workflow execution, distinct from the Harness model-callable JavaScript workflow tool.

## Supporting Harness source evidence

- [Base composition](C:/src/deepseek-harness/packages/bundle/base/cordis.patch.yml), [web composition](C:/src/deepseek-harness/packages/bundle/web-app/cordis.patch.yml) and [standard preset](C:/src/deepseek-harness/apps/cli/config/agent-presets/standard/agent.cordis.yml): service/UI composition, duplicate instances and function-level switches.
- [Tool registry/code modes](C:/src/deepseek-harness/packages/core/tools/src/index.ts), [web function registration](C:/src/deepseek-harness/packages/web/tool-web/src/index.ts), [subagent registration](C:/src/deepseek-harness/packages/subagent/tool-subagent/src/index.ts), [workflow tool](C:/src/deepseek-harness/packages/workflow/tool-workflow/src/index.ts) and [Ralph loop](C:/src/deepseek-harness/packages/workflow/tool-ralph/src/index.ts): callable behavior.
- [Filesystem sandbox](C:/src/deepseek-harness/packages/fs/fs-sandbox/README.md), [observation policy](C:/src/deepseek-harness/packages/fs/fs-observation-policy/package.json), [sandbox-local](C:/src/deepseek-harness/packages/sandbox/sandbox-local/package.json): policy/observation and OS isolation are different components.
- [Badge skill](C:/src/deepseek-harness/packages/skill/skill-badge/README.md), [feedback command](C:/src/deepseek-harness/packages/feedback/command-feedback/README.md), [deliverables plugin](C:/src/deepseek-harness/packages/client/ui-deliverables/src/index.ts), [reference UI](C:/src/deepseek-harness/packages/client/ui-reference/src/index.ts) and [trajectory UI](C:/src/deepseek-harness/packages/client/ui-trajectory/src/index.ts): plugin-specific purposes that are not ordinary tools.

The full checkout also contains opt-in tool packages absent from your pasted inventory (for example terminal, LSP, scheduling and session-query tools). They are outside this list's scope and are not silently counted as enabled. Infrastructure names such as `timer` and `session-query-sqlite` do not, by themselves, expose scheduling or session-query tools.
