<!-- adashi:architecture:begin -->
<!-- adashi:generated revision=589 -->
# Architecture — `desktop/src/chat` (generated)
Generated from the Adashi design model; do not edit, change the model.
These responsibilities are already owned here: extend them, do not duplicate.

- **Chat Workspace** (Component) — Responsibilities: Composes the selected Chat/Run header, semantic timeline, composer, cont…
- **Typed Command & Event Projection Gateway** (Component) — Responsibilities: Sole TypeScript adapter to Trusted Core. Schema-validates versioned comm…

Boundaries crossing this folder:
- Trusted Application Core -> Typed Command & Event Projection Gateway: Returns receipts/snapshots and ordered committed lifecycle/evidence/configuration/repair e…

[Showing 2 of 2 design element(s) bound here, 24 further line(s) dropped. Retrieve the rest with the adashi_design get_scope operation.]
<!-- adashi:architecture:end -->
