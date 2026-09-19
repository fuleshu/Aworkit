# Stable Agent prefixes across user turns

Adashi task #99; owner: Model Call & Internal Agent Loop Orchestrator.

The September 18 production evaluation confirmed that ordinary tool continuations
preserved their provider prefix, but a new user turn replaced upstream planning
output near the beginning of the system message. Requests with hundreds of
thousands of historical tokens consequently lost almost their entire cacheable
prefix despite unchanged tools and instructions.

## Contract

Agent system messages contain frozen workflow instructions, project facts and
tool guidance. Changing upstream graph output is a bounded user-role contextual
message, explicitly labelled as generated context rather than a user instruction.
It enters the existing `ModelToolContextV1` stream at local exchange boundary zero.
The established checkpoint restoration rebases it after previous history and the
new user message, once per node/invocation. Old planning messages keep their
historical positions. Tool continuations, approval resume and process restart
restore that same selected context without duplicate additions or tool replay.
Text-only Agents use the same preparation path.

Text nodes use a node-scoped context invocation identity, including planning
nodes. Sharing the graph-pass identity made a planner followed by a text-only
Agent collide in the existing compression scope registry. Scoping these context
operations fixes that collision without weakening owner validation.

Explicit context edits and compaction retain their existing behavior. Frozen
authority, node ownership, tool definitions and canonical history are unchanged.
Within an edited invocation, previously admitted initial context is not re-added;
the edit can retain or remove it without duplication or resurrection on resume.
This uses the existing checkpoint/event store; it adds no cache, background task,
extra provider call or full-history reconstruction.

## Compatibility and proof

Existing checkpoints remain readable. Their first request after upgrading can
change the old system prefix once to remove the embedded plan. Later new user
turns append context. Manually edited system messages remain user-owned.

Regression tests must compare the actual provider messages and tools across
changing planning output, include text-only Agents and tool continuations, and
verify restart/approval recovery without duplicate graph context. An isolated
native WebView test exercises the real composer with a local HTTP provider.
Exact prefix preservation enables provider cache reuse; it does not guarantee a
DeepSeek hit. Actual hit/miss counters remain the production evidence.

## Verification (September 19, 2026)

- Rust desktop library: 358 passed, one ignored. The new regression covers both
  callable-tool and text-only context, new user turns, store reopen, repeated
  restoration, and edits retaining or removing the admitted plan.
- `desktop/scripts/native-agent-prefix-smoke.mjs`: nine exact provider prefix
  comparisons across three tool-using user turns and two text-only turns. Real
  Shell and Python calls each ran once per turn; restart during approval and
  between completed turns neither repeated planning nor duplicated its context.
- Local native fixture evidence:
  `desktop/src-tauri/target/native-agent-prefix-1789803637966/proof.json`.
  The fixture used no paid provider and made no changes to the live profile.
