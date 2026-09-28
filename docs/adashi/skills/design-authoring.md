<!-- Generated Adashi skill 'design-authoring'; do not edit this copy. -->
# Skill: design authoring

Read this before creating or changing formal design (C4, UML, bindings, mockups). For Markdown
prose and generated architecture blocks, read the `markdown-documents` skill instead.

## Retrieve before you change

- The run.start injection carries a bounded design index: identities, names and versions only. It
  is a list of entry points, not implementation guidance.
- Retrieve the design bound to the files and symbols you are about to change with
  `adashi_design` `get_bindings`, then the relevant scope with `get_scope` or explicit ids with
  `get_by_ids`.
- Align the change with the responsibilities and relationships you retrieve. If the implementation
  needs a different structure, report the mismatch or raise it with the user instead of drifting
  away from the model.
- Design conclusions never belong in chat notes or project memory. Persist them in the model.

## Authoring rules

- `adashi_design` `save` is the only authoring path for agents; never write design files or docs by
  hand. Humans edit official documents in the desktop.
- One transactional `save` call carries every C4/UML/binding change needed for a coherent model.
  An `ok: false` result or a rejected write means nothing was stored.
- `elementType` is `Person`, `Software System`, `Container` or `Component`. A Container requires a
  Software System parent; a Component requires a Container parent. Relationship endpoints,
  diagram attachments and each binding's `designExternalId` must exist in the resulting model.
- A binding target is a file or a symbol. Bind the design to the code it owns so `get_bindings`
  finds it later.
- Choose UML artifacts from the retrieved inventory and the question being modeled: class for
  static structure and contracts, sequence for interactions, flow for workflows, state for
  lifecycles. Attach each typed Mermaid artifact to a C4 element or relationship. Attach nothing
  to a relationship that cannot own it.
- UI mockups are separate SVG artifacts with stable `data-adashi-id` attributes and a positive
  viewport; external resources and scripts are forbidden. Before proposing a revision, read
  `mockup_get_revision_context` and copy the accepted revision into `baseRevision`.

## Editing or deleting existing design

- Read the complete document through `get_by_ids`, `get_scope`, `get_bindings` or `get_documents`
  first. Each `documents` entry contains the full editable `document`, an opaque `documentId` and
  its `readToken`.
- Preserve the fields you intend to keep and copy the `documentId`/`readToken` pairs into the
  write's `readTokens`. Include tokens for dependent documents that a cascading delete removes.
  New identities need no token; Adashi derives dependency checks internally.
- Do not construct a `guard`, read/write sets or `expectedRevision`.
- On `out_of_date`, nothing was saved. Merge your intended edits into the returned full
  `currentDocument`, preserving the other changes, then retry with its new `readToken` and a new
  `operationId`. Never just replace the token on an old payload.
- Overview, index and search snippets are navigation aids, not editable snapshots. A referenced
  document cannot be deleted until its guarded dependents are explicitly unlinked.

Fetch the exact schema with `adashi_help` for `adashi_design` before an unfamiliar operation.
