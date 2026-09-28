<!-- Generated Adashi skill 'task-workflow'; do not edit this copy. -->
# Skill: Task workflow

Read this when creating, updating, finishing or closing a task, or when resolving a task number.

## Identity

- `Task.number` is the current desktop Task #. It is ordered by creation time then permanent id
  across all retained tasks before filtering and pagination, so it can change after a Git update or
  a deletion.
- Resolve a user-supplied `#` with the tasks `get` operation using `taskNumber`, optionally passing
  the `expectedRevision` you observed. Then use the returned permanent `taskId` and `version` for
  every mutation and reference. Never pass a displayed number as `taskId`.

## Lifecycle

- `create` starts a task in `todo`.
- `update` may set `active` from any state, including reopening `closed` work. Returning a task to
  `todo` is forbidden, and `closed` may only be reopened to `active`. A state may be re-set to
  itself.
- `finish` records `active` → `finished` and requires a nonempty `completionMemo` describing what
  changed and how it was verified. Keep the memo lean; evidence lives in design and QA.
- `close` records `finished` → `closed` after review. Closed tasks are accepted history: listings
  and searches leave them out unless you request `closed` explicitly, and a listing reports how
  many it withheld.

## Links and reads

- Link the design specifications a task implements with ordered `designSpecificationLinks`
  (`targetType` element, relationship, uml, mockup or markdown, with an existing stable
  `designExternalId`). Keep the list to the specifications this task implements; supporting
  context belongs in the description or a linked scope.
- `get` returns full evidence. Linked scopes are opt-in with `includeDesignScopes:true`, which
  inlines complete Markdown documents and tokens; retrieve only the one or two branches the work
  needs.
- Lists default to `todo`/`active`/`finished`, limit 25 (1..100). `states: []` selects nothing.

Fetch `adashi_help` for `adashi_tasks` before an unfamiliar operation.
