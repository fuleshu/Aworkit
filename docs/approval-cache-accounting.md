# Approval cost and cache accounting

Adashi tasks #96 (provider usage), #97 (review context), and #98 (permission identity).
Owners: Model Provider Gateway, Authority & Approval Decision Engine, Evidence Inspector.

## Problem and scope

The September 18 investigation found 231 new automatic reviews (about 39 million
input tokens) after description/instruction updates invalidated saved Shell and
Python grants. Review JSON sorted the changing action before the entire tool
history, preventing that large history from forming a reusable prefix. Main-agent
continuations retained their prefixes. Provider cache counters were discarded,
so historical cache performance cannot be reconstructed reliably.

This change retains accounting first, then reduces necessary review context, then
restores compatible user-owned grants. It does not alter Adashi QA jobs or invent
new standing permission from an automatic review.

## Provider evidence

`ModelCacheUsageV1` retains optional cached and uncached input-token counts.
DeepSeek `prompt_cache_hit_tokens` takes precedence over the OpenAI-compatible
`prompt_tokens_details.cached_tokens` alias; aliases are never added together.
`prompt_cache_miss_tokens` is retained independently. Missing, null and invalid
values remain unknown; explicit zero remains zero. Old serialized records remain
readable without migration or guessed cache values.

Text and tool adapters, terminal projections, span usage, context compaction and
independent reviews carry these values. A review observer retains emitted usage
even if response validation or transport subsequently fails. Each actual review
attempt publishes usage once, including cancellation; reusing a stored decision
does not record another billed call. Cancellation does not save an authorization.

Run details show cached/uncached inputs and hit rate, with model/compaction usage
separate from approval reviews. Partial coverage is labelled. Rate denominators
include only calls reporting cached input; no inferred dollar prices are shown.
Actual provider hits are best-effort and must be measured after deployment.

## Reviewer context

The immutable policy and user-visible transcript precede workspace context,
bounded prior evidence and the exact proposed action. Each user message is
preserved in full and stable across successive reviews. Assistant transcript
entries are explicitly untrusted, bounded excerpts. Provider reasoning and opaque
provider context are excluded.

Only the latest eight exchanges of the current invocation are selected from the
existing indexed ledger, newest-first selection followed by chronological
presentation. At most four calls/results per exchange are excerpted. Serialization
stops at the byte limit, avoiding full copies of large historical outputs. Each
exchange is bounded to a 4 KiB JSON excerpt with explicit truncation metadata.
The full review has a 128 KiB bound. If full user constraints and exact action do
not fit, ask the user instead of silently weakening the authorization context.
The reviewer must ask when missing evidence prevents a safe decision.

This bounds cost and keeps the large stable prefix before changing evidence and
action. It does not guarantee a cache hit, and a sliding evidence window can
invalidate the bounded tail. It does not change the main-agent transcript.

## Saved permissions

Native Shell/Python host/start grants use `execution-v1` identity. It excludes
only binding description and tool instructions. Executable, configuration,
schema, limits, approval mode, secret references and all other frozen fields
remain part of the identity. Invocation and replay hashes remain unchanged.
Other tool types retain their previous grant identity.

At store open, only extant native process project grants are eligible to migrate.
The trusted `pipeline.execution-prepared` ledger must contain the same project
key and a binding whose complete old hash equals the saved grant hash. Its typed
execution identity replaces the old identity transactionally. Unsupported or
unproven grants remain unmatched; reviewed actions and approval receipts cannot
create grants. Revocations remain revoked, project boundaries remain enforced,
and authority changes still require approval. Completed/unprovable migration
scans are memoized against their grant set and prepared-history head.

This restores explicit permissions already granted by the user. Unsandboxed host
code without a saved permission still needs the configured approval path. Codex's
sandbox-first execution is not equivalent to silently trusting arbitrary code on
the host.

## Verification

Regression coverage includes provider SSE aliases/zero/unknown, text and tool
projection, durable span accounting, failed review usage, stable review prefixes,
bounded evidence, exact action preservation, legacy grant matching, cross-project
rejection, authority drift and durable revocation. Native smoke verification uses
an isolated profile and local fixture provider, never the user's live database or
paid API. Test outcomes are recorded in the linked Adashi completion evidence.
