<!-- adashi:architecture:begin -->
<!-- adashi:generated revision=4482445221110067 -->
# Architecture — `desktop/src` (generated)
Generated from the Adashi design model; do not edit, change the model.
These responsibilities are already owned here: extend them, do not duplicate.

- **Desktop App Shell & Navigation** (Component) — Responsibilities: Implements the persistent Tauri desktop frame, required left-navigation…

Boundaries crossing this folder:
- Chat Workspace -> Desktop App Shell & Navigation: Requests new/fork/continue Chat navigation after core projection
- Typed Command & Event Projection Gateway -> Desktop App Shell & Navigation: Publishes immutable navigation metadata, appearance, connection, and resync projections

Bound here:
- file `desktop/src/App.tsx`


Markdown design specifications:


- [Menu bar cleanup and About dialog](../../docs/adashi/design-7bc099543cfac12ded4d0e3352b0bb3e81707edf59571f86c74abe20b2e3ed8a.md)
<!-- adashi:architecture:end -->
