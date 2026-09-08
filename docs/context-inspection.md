# Model context inspection and editing

Click the circular context indicator beside Send/Queue to see the current model context footprint, its configured limit, and the system prompt, tools and messages breakdown. When a workflow has multiple model nodes, choose which node to inspect.

The total uses the latest provider-reported input and output counts when available, otherwise a character-based estimate. Section sizes are estimates; they are scaled to the reported total when present. This is not cumulative Run usage. A missing context limit is shown as Unknown, and image costs are excluded from unreported estimates.

**Display Context** opens a large, selectable, structured JSON snapshot. It contains the provider-neutral input messages, frozen tool definitions, completed tool exchanges, injected context and runtime notices. A completed final answer is included after the exchanges. Image content appears as durable references, before native image materialization. Credentials and provider transport configuration are outside this document.

**Enable Edit** unlocks the JSON. Closing with the close button, Save and Close, or Escape validates and saves changes. Invalid JSON, stale context, rejected saves and interrupted requests leave the panel open with the draft intact. Discard changes closes without applying an edit. Formatting-only changes do not create an edit event.

Edits are allowed between turns. They replace the prompt prefix of the selected workflow node for subsequent calls in this Chat, including after restart. Later user/assistant messages and new tool exchanges follow the edited prefix. Past tool calls in that prefix are never executed again. Other Chats and subagent contexts do not inherit the revision. Frozen tool definitions and execution authority cannot be edited here; edit messages, system text, injected context, tool-result content and complete exchanges. Automatic context sources may subsequently add or restore their own required context.

The original conversation and model-call evidence remain immutable. A visible **Context edited** event records the node, base context sequence, before/after hashes and revised document. Native version, Chat-target and context-sequence checks prevent applying a stale panel to newer work. The existing provider and durable-history size limits still apply; the panel is an inspection/research tool, not automatic compaction.

Implementation follows the desktop composer/projection gateway and trusted-core event/history boundaries. Provider adapters preserve assistant/user roles and positioned images across edited tool history.

Validation:

- Frontend context/composer/replay tests in `desktop/src/chat/ContextUsage.test.tsx` and existing Chat integration tests.
- Native context projection, validation, persistence and idempotent edit tests under `runtime/context_inspection` and `runtime/service/tests/context_edit.rs`.
- OpenAI-compatible, Anthropic and Gemini positioned-context mapping tests.
- From `desktop`, run `node scripts/native-context-smoke.mjs` after rebuilding the frontend and native executable. Uses an isolated profile and loopback provider; checks the actual ring, popup, editor, invalid JSON, event, restart, next provider payload, historical tool non-execution, Chat isolation, text-only requests and dark/smaller layout.
