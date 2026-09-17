<!-- adashi:architecture:begin -->
<!-- adashi:generated revision=616 -->
# Architecture — `desktop/src-tauri/src/runtime/service` (generated)
Generated from the Adashi design model; do not edit, change the model.
These responsibilities are already owned here: extend them, do not duplicate.

- **Chat/Run Lifecycle Service** (Component) — Responsibilities: Owns command handling and legal transitions for the one-Chat/one-Run/ses…

Boundaries crossing this folder:
- Desktop Command & Event API -> Chat/Run Lifecycle Service: Dispatches idempotent versioned Chat/Run commands and receives domain rejection or committ…
- Chat/Run Lifecycle Service -> Canonical Event Commit & History Router: Atomically commits versioned lifecycle event batches, snapshot/checkpoint references, comm…

[Showing 1 of 1 design element(s) bound here, 9 further line(s) dropped. Retrieve the rest with the adashi_design get_scope operation.]
<!-- adashi:architecture:end -->
