<!-- adashi:architecture:begin -->
<!-- adashi:generated revision=632 -->
# Architecture — `desktop/scripts` (generated)
Generated from the Adashi design model; do not edit, change the model.
These responsibilities are already owned here: extend them, do not duplicate.

- **Chat/Run Lifecycle Service** (Component) — Responsibilities: Owns command handling and legal transitions for the one-Chat/one-Run/ses…
- **Harness Context Revision & Lineage Store** (Component) — Responsibilities: Maintains the Run's structured, inspectable Harness Context as logical i…

Boundaries crossing this folder:
- Desktop Command & Event API -> Chat/Run Lifecycle Service: Dispatches idempotent versioned Chat/Run commands and receives domain rejection or committ…

[Showing 2 of 2 design element(s) bound here, 19 further line(s) dropped. Retrieve the rest with the adashi_design get_scope operation.]
<!-- adashi:architecture:end -->
