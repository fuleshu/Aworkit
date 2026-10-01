<!-- adashi:architecture:begin -->
<!-- adashi:generated revision=3312827354940754 -->
# Architecture — `desktop/src-tauri/src/runtime/service` (generated)
Generated from the Adashi design model; do not edit, change the model.
These responsibilities are already owned here: extend them, do not duplicate.

- **Chat/Run Lifecycle Service** (Component) — Responsibilities: Owns command handling and legal transitions for the one-Chat/one-Run/ses…
- **Recovery & Rehydration Coordinator** (Component) — Responsibilities: Rebuilds non-terminal Chat aggregates after core/worker/host restart, re…

Boundaries crossing this folder:
- Desktop Command & Event API -> Chat/Run Lifecycle Service: Dispatches idempotent versioned Chat/Run commands and receives domain rejection or committ…

[Showing 2 of 2 design element(s) bound here, 25 further line(s) dropped. Retrieve the rest with the adashi_design get_scope operation.]
<!-- adashi:architecture:end -->
