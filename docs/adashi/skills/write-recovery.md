<!-- Generated Adashi skill 'write-recovery'; do not edit this copy. -->
# Skill: Write recovery

Read this when an Adashi write is rejected. A rejected write is **not persisted**: correct the
reported parameters and submit a new call. Never report completion for a rejected write.

## Read the rejection, do not guess

Adashi returns the actual reason, the missing and unused parameters, the original evidence, and the
complete operation schema. Use the returned schema instead of a remembered one, and copy opaque ids,
versions and tokens from real reads.

- **Missing `operationId`** — every mutation needs a unique idempotency key. Reuse it only for an
  identical retry of the same request.
- **Unknown or missing parameter** — the report lists the fields the selected operation accepts.
  Fields from another operation are rejected rather than ignored.
- **Unknown operation or change type** — re-read `adashi_help` for the tool; the catalog lists the
  valid operations, and design save lists `availableChangeTypes`.
- **Stale `expectedVersion`** — reread the resource (task, QA job, rule, memory) and retry with the
  version from that read, never the project revision. On a conflict nothing was written.
- **`out_of_date` on a design write** — merge your intended edits into the returned
  `currentDocument`, preserving everything else, then retry with the new `readToken` and a new
  `operationId`.
- **Missing design reference** — a binding, relationship endpoint, diagram attachment or delete
  dependency names a target that does not exist in the resulting model. Create or link the target
  in the same transaction, or remove the reference.
- **Reused `operationId` with different arguments** — the key already belongs to another request.
  Submit the corrected write under a new key.
- **Uncertain commit** — do not retry blindly. Reread the resource; if the write landed, continue,
  otherwise retry with the same `operationId` so the retry stays idempotent.

## Contract violations to avoid

- Design saves are whole-document upserts: preserve fields you intend to keep.
- Rule and memory updates need the resource version from retrieval, not the project revision.
- A QA job must declare `kind` and `scope`, stay inside its kind's timeout ceiling, avoid packaging
  outside `kind=release`, and link what it verifies.
- If a call fails because the MCP surface is not configured, continue without Adashi and mention
  the limitation only when it affects the outcome. Any other failure is real: report it rather than
  working around it.
