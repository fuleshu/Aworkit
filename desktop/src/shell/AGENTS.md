<!-- adashi:architecture:begin -->
<!-- adashi:generated revision=683 -->
# Architecture — `desktop/src/shell` (generated)
Generated from the Adashi design model; do not edit, change the model.
These responsibilities are already owned here: extend them, do not duplicate.

- **Desktop App Shell & Navigation** (Component) — Responsibilities: Implements the persistent Tauri desktop frame, required left-navigation…

Boundaries crossing this folder:
- Chat Workspace -> Desktop App Shell & Navigation: Requests new/fork/continue Chat navigation after core projection
- Typed Command & Event Projection Gateway -> Desktop App Shell & Navigation: Publishes immutable navigation metadata, appearance, connection, and resync projections

Bound here:
- file `desktop/src/shell/NavigationPane.tsx`

[Showing 1 of 1 design element(s) bound here, 9 further line(s) dropped. Retrieve the rest with the adashi_design get_scope operation.]
<!-- adashi:architecture:end -->
