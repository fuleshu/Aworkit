<!-- Generated Adashi skill 'retrieval'; do not edit this copy. -->
# Skill: Searching project context

Adashi is addressable the same way you grep the codebase, so reach for it the same way.

- `adashi_grep` searches design, tasks and memory at once. Its pattern is case-insensitive;
  whitespace-separated terms are AND; a quoted phrase is an exact substring. `in:`, `file:`,
  `type:`, `state:` and `limit:` narrow the search, and anything else is searched as text.
- An empty pattern returns the top-layer overview with counts — the cheapest way to find out what
  a project contains before you know any identity.
- Each result line begins with a drillable locator. `design:<externalId>` opens the design
  `get_scope` operation, `task:<id>` opens the tasks `get` operation, and `memory:<noteId>` opens
  the memory `get` operation with its `noteId` filter.
- Markdown title/body matches use the same `design:<externalId>` locator; `type:markdown` narrows
  to prose. A `markdown:<externalId>` documentId from canonical retrieval opens through
  `get_documents`. Never substitute the generated filename for either identity.
- `file:<path>` scopes a search to a file instead of the whole project: it keeps the design
  elements bound to that path and the tasks whose file lists contain it, so the results are the
  things that own or touched the file you are editing. It is a scope, not a query of its own — a
  `file:` clause with nothing to search for returns the overview, so always pair it with at least
  one term (`concurrency file:src-tauri/src/concurrency.rs`).
- Output is bounded and reports `showing N of M — narrow the query` when it is. Narrow with the
  pattern or the clauses rather than raising the limit.

`adashi_grep` does not search QA or rules. Read those through their own tools, or fetch exact
syntax with `adashi_help` for `adashi_grep`.
