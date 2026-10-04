# Aworkit chat history schema

The canonical chat history database (`history/aworkit.sqlite3`) as this build
creates it: SQLite `user_version` 3, WAL journal, foreign keys on, every table
`STRICT`.

The table list below is the authority the drift check compares against the real
database this build creates - when a table or column is added, renamed or
removed, this file must change with it.

| table | columns |
| --- | --- |
| `chat_streams` | `chat_id`, `branch_id`, `run_id`, `head_sequence`, `aggregate_version` |
| `semantic_events` | `event_id`, `chat_id`, `branch_id`, `sequence`, `schema_version`, `kind`, `payload` |
| `attempts` | `attempt_id`, `chat_id`, `branch_id`, `operation_id`, `ordinal`, `outcome_class` |
| `checkpoints` | `chat_id`, `branch_id`, `committed_sequence`, `reducer_version`, `state_hash`, `frozen_snapshot_ref` |
| `deduplication` | `key_type`, `key`, `request_hash`, `chat_id`, `branch_id`, `receipt` |
| `delivery_outbox` | `outbox_id`, `chat_id`, `branch_id`, `commit_sequence`, `delivery_cursor`, `destination`, `schema_version`, `payload`, `payload_hash`, `delivered` |
| `prepared_artifacts` | `token_id`, `artifact_id`, `content_hash`, `byte_size`, `media_type`, `logical_name`, `staging_generation`, `prepared_at_epoch_ms`, `finalized_event_id` |
| `artifacts` | `artifact_id`, `content_hash`, `byte_size`, `media_type`, `logical_name`, `created_generation`, `created_at_epoch_ms`, `retention_class`, `availability` |
| `artifact_references` | `artifact_id`, `origin_event_id` |
| `store_state` | `key`, `value` |

## Keys, indexes and constraints

- `chat_streams` primary key `(chat_id, branch_id)`; `semantic_events` unique
  `(chat_id, branch_id, sequence)`; `attempts` unique
  `(chat_id, branch_id, operation_id, ordinal)`; `checkpoints` primary key
  `(chat_id, branch_id, committed_sequence)`; `deduplication` primary key
  `(key_type, key)`; `artifact_references` primary key
  `(artifact_id, origin_event_id)`.
- Every history table except `deduplication` and `store_state` references
  `chat_streams(chat_id, branch_id)`, so a stream cannot be deleted while its
  events exist.
- Indexes: `delivery_outbox_cursor` (unique on `delivery_cursor`),
  `delivery_outbox_pending` (`delivered`, `delivery_cursor`),
  `semantic_events_stream` (`chat_id`, `branch_id`, `sequence`),
  `semantic_events_kind` (`chat_id`, `branch_id`, `kind`, `sequence`),
  `semantic_events_span_lookup` on `json_extract(payload, '$.spanId')` for
  `span.*` rows, and `prepared_artifacts_age`
  (`finalized_event_id`, `prepared_at_epoch_ms`).

## Disposable projection database

A separate, rebuildable `projection.sqlite` holds `chat_projection`,
`timeline_projection`, `evidence_projection`, `artifact_projection`,
`projection_state`, `projection_cursor` and `projection_health`, plus the FTS5
table `search_projection` (with SQLite's own shadow tables). None of it is
canonical: Aworkit rebuilds it from `semantic_events`, and storage maintenance
treats the file as disposable rather than quarantining it when it is corrupt.
