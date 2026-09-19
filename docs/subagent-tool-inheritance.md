# Native subagent tool inheritance

Implements Adashi task 101 (id 102), design `aworkit.workflow_worker.subagent`
and `aworkit.desktop_ui.conversation`.

`spawn_subagent` defaults to implementation access: the child receives the
delegating Agent node's exact frozen tool selection, except `tool.subagent`.
Enabled Settings tools and tools selected only by other workflow nodes are not
inherited. Optional `readOnly: true` narrows this selection to the established
native research tools and MCP tools declaring both `readOnlyHint: true` and
`destructiveHint: false`. MCP auto-approval is not a read-only classification.

The trusted context records node and tool identities once per Agent invocation.
Children reuse the same frozen gateway, tool definitions, options, workspace,
credentials and broker. This metadata is durable but is not added to model
messages. Tool schemas and instruction order remain stable between child turns;
there is no additional scope-selection or approval-model request.

The child may use already-permitted operations: workspace file access, explicit
tool policy, Chat Full access, MCP auto-approval, and applicable standing grants.
An operation needing new approval is durably recorded as not executed. The
child immediately returns `status: parent_approval_required` and the exact
`blockedActions` to its parent. Remaining calls in that provider batch are not
executed. The transcript still contains a result for every proposed call.
The parent can propose the blocked action through its normal approval policy;
no approval is bypassed and no child approval-model call is made.

Jobs remain owned by the Chat, with their originating Agent/child invocation
persisted separately. A child can list and control only its own jobs; the parent
can manage every job in the Chat. The child's completion check only considers
its own jobs, and Stop still cancels the Chat's process trees. A handoff returns
the child's job inventory so the parent can finish supervising it.

Each child has explicit role instructions, its own model context, and delegated
workspace/skill contributions. Native subagent span ancestry identifies child
speech and tool activity as subagent-owned in both streaming and replay.

New Chats freeze `inheritParentTools: true`. Existing frozen Chats retain their
original read-only authority and definitions. Old Settings remain readable and
acquire the new contract when starting a new Chat; continuing a stopped Chat
does not mutate its authority. Recursive delegation remains unavailable.

Adashi task 102 (id 103), linked to `aworkit.trusted_core.snapshot_freezer`
and `aworkit.workflow_worker.subagent`, removes the continuation re-freeze gate.
Only a new Chat compiles and freezes configuration. Later input and Stop/Continue
reuse the persisted snapshot, manifest, provider, workspace and tool bindings
directly, with fresh pass state and messages. Catalog or compiler changes and
echoed desktop metadata cannot invalidate an existing Chat or replace its saved
configuration. Explicit Chat approval-mode changes still apply. Duplicate
commands reuse their recorded results and must retain their original input
and Chat ownership.

The native host supports the exact pre-`readOnly` subagent descriptor at tool
dispatch while retaining that binding's original child scope and argument
validation. This permits old Chats to keep using delegation after an upgrade.
Operation permissions continue to apply when an operation is dispatched.

Behavioral tests cover real child file writes/edits, shell and Python execution,
MCP dispatch, exact parent selection, explicit research filtering, approval
handoff and parent resumption, replay, sibling job isolation, Chat cancellation,
legacy delegation, and UI actor attribution. Scripted providers avoid API costs.
