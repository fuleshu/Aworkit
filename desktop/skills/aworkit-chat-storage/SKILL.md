---
name: aworkit-chat-storage
description: Where Aworkit stores chats and what those files hold - the local SQLite semantic history (streams, events, attempts, checkpoints, deduplication, outbox), the disposable read-only projections, the real table schema, and the portable-session format.
---

# How Aworkit stores chats

A chat is stored locally. There is no account and no server copy: the desktop
app writes one SQLite database per profile, and that database is the canonical
record of every Chat, Run and turn.

## 1. Where the files are

Everything lives under Aworkit's application data directory followed by
`runtime` - on Linux `~/.local/share/com.aworkit.desktop/runtime`, on macOS
`~/Library/Application Support/com.aworkit.desktop/runtime`, on Windows
`%APPDATA%\com.aworkit.desktop\runtime`.

| path | what it holds |
| --- | --- |
| `history/aworkit.sqlite3` | **Canonical semantic chat history** - the answer to "where is my chat stored?" |
| `history/aworkit-invocations.sqlite3` | Durable tool-invocation records, approvals and their grants |
| `history/shell-jobs/` | Working directories with captured output of shell and Python jobs |
| `documents/` | Schema-versioned JSON documents: settings and the workflow library |
| `tool-plugins/` | Installed plugin packages (see the `aworkit-plugins` skill) |
| `core/projects/` | Project coordination state |
| `projection.sqlite` | A throwaway read-only projection of history: safe to delete, Aworkit rebuilds it |

Deleting the projection changes nothing. Deleting `history/aworkit.sqlite3`
deletes the chats.

## 2. What the history database holds

Identity is deliberately flat: one Chat is one Run is one stored session, so
`chat_id` is the durable identity, `branch_id` selects a branch of it, and every
row is keyed by that pair. The database is SQLite in WAL mode with foreign keys
on, schema `user_version` 3.

The exact tables and their columns are documented, and checked against the
build, in `references/schema.md`. In outline:

- **`chat_streams`** - one row per (chat, branch): the `run_id`, the current
  `head_sequence`, and a monotonic `aggregate_version`.
- **`semantic_events`** - the append-only history itself. Each row has a stable
  `event_id`, its position (`sequence`, unique and contiguous from 1 within the
  chat/branch), a `kind`, a payload `schema_version` and a JSON `payload`.
- **`attempts`** - one row per execution attempt of an operation, with its
  `ordinal` and `outcome_class`. A retry adds an attempt; it never rewrites an
  earlier one.
- **`checkpoints`** - reducer checkpoints: the committed sequence, the
  `reducer_version`, a `state_hash`, and the `frozen_snapshot_ref` of the
  per-Chat frozen Run snapshot.
- **`deduplication`** - the idempotency ledger for commands and invocations
  (`key_type`, `key`, `request_hash`, the receipt that was returned). This is why
  resending the same command is safe.
- **`delivery_outbox`** - effects committed with an event and dispatched
  afterwards, with a delivery cursor and a payload hash, so a crash between
  commit and delivery is recoverable without repeating anything.
- **`prepared_artifacts`**, **`artifacts`**, **`artifact_references`** - content
  addressed files (images, large payloads) with size, media type, availability
  and the event that produced them; `store_state` holds store metadata.

The event kinds group by prefix: `chat.*` for lifecycle (started, turn stopped,
cancelled, execution context frozen), `span.*` for activities
(`span.started`, `span.content_delta`, `span.completed`, `span.failed`,
`span.cancelled`, `span.usage`), and `pipeline.*` for harness records such as
`pipeline.tool-invocation-prepared`, `pipeline.provider-outcome`,
`pipeline.model-tool-exchange`, `pipeline.tool-outcome`,
`pipeline.approval-resolved` and `pipeline.skill-context`. Payloads are Aworkit's
own JSON DTOs, redacted; credential values never appear.

Only the trusted core appends. A commit batch (events, attempts, checkpoints and
outbox rows) lands atomically and is durable before anything is dispatched, and
`head_sequence` only ever advances. If integrity, schema or sequence continuity
fails, the store enters an explicit read-only quarantine instead of guessing a
head or repairing rows.

## 3. Projections

The store also maintains a disposable read-only projection (`projection.sqlite`)
used for fast listing and search: `chat_projection`, `timeline_projection`,
`evidence_projection`, `artifact_projection`, a `search_projection` FTS5 index,
plus `projection_state`, `projection_cursor` and `projection_health`. It is a
cache with no authority - it is rebuilt from the semantic events, and repair
treats it as discardable.

## 4. Inspecting a chat

The canonical rows are plain SQLite, so the `sqlite3` CLI works:

```sh
DB=~/.local/share/com.aworkit.desktop/runtime/history/aworkit.sqlite3
sqlite3 "$DB" 'select chat_id, branch_id, head_sequence from chat_streams;'
sqlite3 "$DB" "select sequence, kind from semantic_events
               where chat_id='<chat id>' and branch_id='<branch id>'
               order by sequence limit 20;"
```

Open it read-only while Aworkit runs; the app holds it in WAL mode and a
long-running write lock from another process would only slow both down. For a
human answer prefer the app's own **Run details** inspector, which reads the
same rows.

## 5. Portable sessions

Aworkit also defines a portable session format for carrying history between
machines: family `aworkit-portable-session`, version 1.0. It is an immutable,
content-addressed object store - objects are published under
`segments`, `checkpoints`, `artifacts`, `manifests` and `claims` namespaces and
never modified in place.

An active portable Chat keeps one machine-local, explicitly **non-canonical**
journal table (`portable_runtime_journal_v2`) recording the two-store commit
fences with their phase (`pending`, `linked`, `quarantined`). It carries
publication and recovery facts only - never a second copy of the history - and
becomes inert when the portable head it was bound to no longer matches.

In this build the portable-history option is retired: the v1 setting
`portableHistoryEnabled` is migrated to `false` and cannot be turned back on, so
local SQLite is the canonical backend for a chat and the portable format is the
export/import contract rather than a live storage mode.
