# Concurrent desktop Chats

Implemented against Adashi task #88, `uml.chat.concurrent_execution`, and the
Chat Lifecycle, Desktop API, Invocation Broker, Projection Gateway, Chat
Workspace, and App Shell components. Adashi owns the formal design.

## Reference and implementation

The DeepSeek Harness reference separates the selected session from resident
session drivers:

- `packages/core/agent-loop/src/agent.ts`: driver, inbox, and cancellation per session.
- `packages/api/session-controller/src/client/sessions/manager.ts`: session map and independent selection.
- `packages/api/session-controller/src/client/sessions/service.ts`: subscription lifetime separate from execution.
- `packages/client/ui-workspace/src/client/rows/Rows.tsx`: clickable rows with running status.

Aworkit's former full-turn desktop mutex and selection-dependent history binding
prevented this behavior. The native command dispatcher now reserves the target
Chat, builds a worker bound to its immutable identity, and releases the desktop
coordinator before model/tool execution. Different Chats can progress together;
mutations within a Chat remain serialized. Short shared locks protect index and
frozen-session writes. Worker feedback applies to the current Settings document.

Every live model and tool dispatch handles only its own invocation. Draining the
shared outbox could otherwise misclassify another Chat's active dispatch as an
interrupted effect. Durable recovery and frozen authority still use the existing
broker and canonical event history.

The UI captures Chat targets before asynchronous work and fences snapshots by
navigation generation and response order. It preserves live events that arrive
while a full snapshot is in flight. Drafts, pending commands, errors, maintenance
queues, and retries belong to their Chat. History rows show a small accessible
running icon; New Chat and selection remain available during execution and
recovery. Delete and fork reject an active target.

## Verification

From `desktop`, build the frontend with `npm run build`, then build the native
binary with `cargo build --manifest-path src-tauri/Cargo.toml`. Run
`node scripts/native-chat-concurrency.mjs` on Windows with WebView2 installed.
The script creates an isolated profile and local SSE provider, uses real Tauri
IPC and WebView controls, and writes its result and screenshots beneath
`src-tauri/target/native-concurrency-<timestamp>`.

It checks overlapping provider calls, two busy icons, New Chat and history
navigation, draft retention, Stop isolation, Settings preservation during a
background completion, exact event isolation, and recovery after restart without
automatic effect replay. It also verifies that an interrupted Chat does not block
a new one. Targeted Rust tests exercise worker ownership and invocation-specific
tool delivery; UI tests cover delayed snapshots and per-Chat queue retries.

The SSE fixture sends heartbeats while terminal responses are held. The existing
synchronous provider reader observes cancellation between reads; this change
does not add immediate interruption of a silent provider socket. Draft retention
is scoped to the desktop workspace lifetime, while committed history survives
restart.
