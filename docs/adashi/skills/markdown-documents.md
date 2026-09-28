<!-- Generated Adashi skill 'markdown-documents'; do not edit this copy. -->
# Skill: Markdown documents

Read this when writing or editing Markdown design prose, or when you touch generated
`docs/adashi` output or an `AGENTS.md` architecture block.

## What Markdown designs are for

Use a Markdown document for narrative specifications, decisions, constraints and explanations that
diagrams cannot express. A document may stand alone at project scope with no invented C4 parent, or
carry ordered `designLinks` to elements, relationships, UML, mockups and other Markdown documents.
Task and QA references use `{"targetType":"markdown","designExternalId":"<stable externalId>"}`.
Titles and generated file paths are not identity; navigate by the stable `externalId`.

## Authoring

Fetch the exact schema first:

```json
{"tool":"adashi_design","operation":"save","changeTypes":["upsert_markdown"]}
```

Create a document (no read token is needed for a new identity):

```json
{"projectName":"Your project","operation":"save","operationId":"create-checkout-design","changeIntent":"Record checkout requirements","changes":[{"op":"upsert_markdown","externalId":"checkout-design","title":"Checkout requirements","body":"# Checkout\n\nPreserve the draft when validation fails.\n","designLinks":[]}]}
```

Read a complete document and its token:

```json
{"projectName":"Your project","operation":"get_documents","ids":["markdown:checkout-design"]}
```

Update it (the update sends a complete object; preserve every field and association that should
remain):

```json
{"projectName":"Your project","operation":"save","operationId":"update-checkout-design","changeIntent":"Clarify validation feedback","readTokens":[{"documentId":"markdown:checkout-design","readToken":"COPY_FROM_COMPLETE_CANONICAL_READ"}],"changes":[{"op":"upsert_markdown","externalId":"checkout-design","title":"Checkout requirements","body":"# Checkout\n\nPreserve the draft when validation fails. Show feedback beside the field.\n","designLinks":[]}]}
```

- `list_markdown` with `markdownQuery` returns bounded metadata, filters and continuation pointers.
- `get_scope` with the external ID opens a document with its explicit associations and backlinks.
- Task retrieval expands attached designs with `includeDesignScopes:true`; read each attached
  document completely before implementing it.
- On `out_of_date` nothing was saved: merge into the returned `currentDocument`, then retry with
  the new token and a new `operationId`.

## Generated files and architecture blocks

- Generated architecture blocks appear between the raw markers `adashi:architecture:begin` and
  `adashi:architecture:end`. Adashi renders them from the model, so a hand edit is transient.
  Change the model with `adashi_design` `save`, never the block.
- Treat the block as the model's own statement of what that folder owns: extend those
  responsibilities and do not build a parallel mechanism for something already owned.
- Opted-in projects also generate complete Markdown files and an index under `docs/adashi` (or the
  configured output directory), and mirror the Adashi skills under `docs/adashi/skills`.
- Read generated files naturally for discovery, then retrieve the fresh complete canonical
  document and `readToken` through MCP before every edit. Generated notices, source fingerprints
  and revisions are freshness metadata, not write tokens, and must not be copied into canonical
  content. Direct file edits are reported as drift and overwritten; files are never silently
  imported.
