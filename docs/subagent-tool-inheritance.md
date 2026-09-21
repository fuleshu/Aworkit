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

## Continuable, forked and controllable children

Adashi task 112 (id 112), design `aworkit.workflow_worker.subagent`,
`aworkit.workflow_worker.context_store`, `aworkit.workflow_worker.limits` and
`aworkit.workflow_worker.suspension`, extends delegation beyond the one-shot
child. The inherited-tool contract is unchanged; the new behavior is additive
and every new tool is a separate frozen binding.

Each child now owns a durable `ChildContextFrameV1` in the Chat's machine-local
operational record store: child identity, delegating Agent node, parent
invocation, lineage, depth, declared inherited identities, read-only mode, the
immutable prompt prefix, the child's own committed exchanges, status, counters,
result text, blocked approval actions and timestamps. Later revisions of one
child supersede earlier ones, so a Run resumes a child from its newest head.
Frames are keyed by child id and revision: a replayed pass re-appends at most the
same revision and never rewrites an acknowledged child head.

`tool.subagent` still runs the child to completion synchronously, but the result
now carries `childId`, `status`, `headRevision` and the child's own job
inventory. A settled child stays resumable for the rest of the Run.

`tool.subagent_fork` creates a child from a declared, bounded projection of the
delegating Agent's committed conversation rather than a standalone brief. The
projection is part of the frozen delegation contract: `forkMaximumItems` and
`forkMaximumBytes` bound how many recent conversation items are inherited, the
oldest are omitted first, and the newest item always survives. System
instructions are never inherited. The projection hashes deterministically, so
the same parent revision and the same frozen bounds always produce the same
child prefix. The projection is captured once per model turn before dispatch, so
every call in a turn forks the same deterministic prefix.

`tool.subagent_list`, `tool.subagent_message` and `tool.subagent_cancel` are the
owner-isolated controls. They resolve the delegating Agent from the same frozen
identity scope a spawn uses, then restrict to children of that Chat, Run and
Agent node. A child never inherits any delegation or control tool, so a child
cannot delegate again or reach a sibling. `subagent_message` resumes the child's
own conversation at its committed head with one new user message and returns a
new outcome; the child's prompt prefix, inherited tools and authority stay
identical across turns. `subagent_cancel` closes the scope and stops the jobs
the child still owns; repeating it for the same child is a no-op success. A
cancelled child cannot be continued and reports `cancelled` without an error.

Operator isolation and failures. A child call still uses the existing durable
broker, filesystem boundaries and standing grants. `parent_approval_required`,
contract failures and provider failures remain the parent's decision under the
frozen attempt policy; an uncertain child outcome is never retried
automatically. A refused spawn or a failed child turn is a failed tool result
the parent model can read, not a node failure.

Limits are admitted before any child context exists and charged to the parent
scope. `maximumDepth` (default 1) keeps nested delegation unavailable, exactly
as the approved inheritance contract states, and refuses a child whose depth
would exceed it. `maximumChildren` (default 32) counts the Agent's live
(cancelled children excluded) scopes and refuses the spawn that would exceed it.
`subagent_list` shows the durable identities, so an exhausted bound is
recoverable by cancelling a finished child instead of guessing. There is still
no model-turn, elapsed-time or request-size cap: the provider and the model's
context window remain the only budgets.

Evidence and recovery. The existing subagent spans and cards are unchanged, and
the parent transcript still receives only the declared child outcome. Because
the frame and every child tool settlement are durable and keyed, recovery
restores child lineage and committed invocation ids and never re-runs an
acknowledged child effect. Replayed commands reuse their committed provider
responses, and a continuation after replay still resumes from the same head.

Tests cover real continuation across turns with a stable child prefix, fork
projection inheritance and reproducibility, the frozen projection bound, list /
cancel idempotency and closed-scope messaging, unknown-child refusal, fan-out
and depth exhaustion before child creation, and replay without re-execution.

## Background child jobs

Adashi task 119 (id 118), design `aworkit.workflow_worker.subagent`,
`aworkit.workflow_worker.limits` and `aworkit.workflow_worker.suspension`,
removes the blocking child. Delegation reuses the existing Chat job registry
instead of introducing a second scheduler.

The job registry now has a runner abstraction. An entry is either an OS process
session (shell/Python) or an in-process delegated child run; the persisted
record carries a `kind` that decodes to `process` for legacy rows. Both kinds
share the same durable identity, Chat and child ownership scope, running and
terminal snapshot, captured output cursor, `job_output`/`job_input`/`job_stop`/
`job_keep`/`job_list` control surface, resolution gate and completion barrier.

A new Chat freezes `runInBackground: true` on `tool.subagent`,
`tool.subagent_fork` and `tool.subagent_message`. A delegation therefore computes
its child scope and frame first, commits a `running` frame, registers a child job
under the spawning invocation and returns `{childId, jobId, running: true}`
immediately; the child loop runs on its own thread. One call may pass
`runInBackground: false` to wait inline, which is what the inheritance and
continuation suites do to keep their scripted dialogues deterministic.

Observing and steering. `job_list` reports child jobs with `kind: subagent` and
their `childId`; `job_output` returns the child's live progress (one line per
model turn, with the child's own counters under `child`) and, once settled, the
final outcome. `job_input` steers a running child at its next step boundary by
queueing a context message the child's tool port drains; for a settled child it
resumes the conversation as a new background turn-set. `job_stop` cancels the
child's own cancellation token, waits for the terminal snapshot and closes the
child scope, so a stopped child can never be resumed accidentally. `job_keep`
lets the delegating Agent finish its turn while the child keeps working.

Evidence. A background child owns its evidence stream: `RunEventStream` gains a
detached child constructor whose span root carries the child identity, and the
model loop accepts an evidence observer from the invocation port. This keeps the
child's model and tool spans out of the delegating pass's span tree, so the
cardinal rule that a committed parent span cannot terminate while a committed
child span is open holds even though the child outlives the pass. The child still
commits to the same Run history, its root span is attributed to the subagent
actor, and its frame records the lineage back to the delegating tool.

Limits and recovery. Child jobs count toward the registry's running capacity.
The completion barrier treats an unresolved child like an unresolved process
unless it was explicitly kept. On restart a running child job becomes
`interrupted`, is never replayed, and its frame is reported as interrupted rather
than running; a continuation then starts a fresh turn-set from the child's
committed head, and settled child tool effects are re-delivered from the ledger
rather than executed again.

Tests cover a background delegation returning a job ticket with live per-turn
progress, steering at the next step boundary, stopping and closing the scope,
and keeping a running child while the parent finishes its turn. The registry's
own tests cover child job identity, keep/stop, ownership isolation and restart
interruption.

## Subagent chat tabs

Adashi task 120 (id 119), design `aworkit.desktop_ui.chat_workspace`,
`aworkit.desktop_ui.conversation`, `aworkit.desktop_ui.composer`,
`aworkit.desktop_ui.settings` and `aworkit.desktop_ui.projection_gateway`.

Tabs are transient presentation state over one Chat, one Run and one canonical
history. The parent Chat is the first, permanent, non-closable tab; every
delegated child is one closable tab identified by its durable `childId`. Closing
a tab only hides it and never cancels the child, and no tab creates a second Run,
session or history. The open set is remembered per Chat for the process, so
switching Chats switches tab sets and returning rebuilds the child from the same
bounded, back-scrollable window that renders the main feed. The strip is bounded
(evicting the oldest non-active child) and scrolls horizontally on overflow.

Core surface. The desktop no longer derives children from model-facing tools.
`RuntimeSnapshot` gains an authoritative `subagents` catalog read from the
durable `pipeline.subagent-child` frames. Each entry carries child and parent
identities (`childId`, `nodeId`, `parentInvocationId`, `parentCallId`), kind,
status, running flag, task, assigned context, settled answer and counters. A
frame whose job is no longer live is reported `interrupted`, so a restart never
shows a phantom running child. A child's evidence is read from the same history
by a child-scoped page: every fact a detached child stream commits is tagged
with `subagentChildId`, so `desktop_chat_events` accepts an optional `childId`
and returns only that child's facts while stepping the raw cursor back exactly
like an older page. Inline children now use the same detached evidence stream as
background children, which is what makes one child's activity attributable
without keeping a parent span open across passes.

Reading a child scope. The Chat feed is a bounded *recent* window, so a child's
earlier facts — including the `span.started` records its cards need — are often
outside it, especially after the window is reloaded on navigation or a fresh
open. A child tab therefore pulls its own newest page as soon as it becomes
active instead of waiting for a scroll that an empty list can never produce, and
keeps stepping the raw cursor back on demand until an earlier child activity
appears or history is exhausted. Until that read settles the tab reports that it
is loading; a failed read settles with its error and an explicit retry rather
than becoming an unbounded retry loop; and a settled child that genuinely
committed no rendered activity still shows its answer through the normal markdown
renderer instead of as bare text.

Desktop behavior. The Chat Workspace owns the tab strip; the conversation center
is a `tabpanel` whose active tab selects either the parent timeline or the
read-only child view. The child view shows a lineage header (kind, child id,
delegating node, counters), the assigned task and context, and the child's own
activity through the existing timeline renderer; it has no composer, no steering
and no approval controls. A `tool.subagent` activity block offers an "Open
subagent" action that opens or activates that child's tab, and the composer shows
a subagents button beside the goal control, enabled exactly when the Chat owns a
child and listing them running-first.

Settings. `SubagentViewConfigurationV2` is a global user preference committed
through `desktop_subagent_view_commit`; it is never part of a Chat's frozen tool
contract, a restart preserves it, and a generic full-document Settings save that
carries the default keeps the stored value. `autoOpen` gives a newly created
child a background tab without moving focus; `autoClose` closes a child's tab
when it reaches a terminal state only while that tab is open and inactive. A tab
closed by hand stays closed.

Accessibility. The strip is a labelled `tablist` with roving `tab` selection,
labelled close buttons, arrow/Home/End navigation, Escape dismissal and focus
restoration for the picker dialog, a polite live region for the preference
status, forced-colors borders and reduced-motion handling.

Tests cover the tab-state machine (dedupe, activation, close, bounded overflow,
per-Chat memory, auto-open without focus change, auto-close only when inactive),
the strip and picker contracts, the authoritative catalog merge (including an
authoritative interruption that a stale running fact cannot overwrite), the
child-scoped feed filter and paging, the workspace flow from the delegating tool
block to the read-only child view, the Settings preference and its restart and
generic-save durability, and the Rust catalog and child-page reads.
