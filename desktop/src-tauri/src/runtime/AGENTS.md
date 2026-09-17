<!-- adashi:architecture:begin -->
<!-- adashi:generated revision=624 -->
# Architecture — `desktop/src-tauri/src/runtime` (generated)
Generated from the Adashi design model; do not edit, change the model.
These responsibilities are already owned here: extend them, do not duplicate.

- **Chat/Run Lifecycle Service** (Component) — Responsibilities: Owns command handling and legal transitions for the one-Chat/one-Run/ses…
- **Capability Invocation Broker** (Component) — Responsibilities: Receives worker capability proposals, obtains the sole core authority de…
- **Local State and Evidence Store** (Container) — The machine-local persistence boundary contains canonical semantic Chat/Run events and ope…

Bound here:
- file `desktop/src-tauri/src/runtime/cancellation.rs`

[Showing 3 of 3 design element(s) bound here, 31 further line(s) dropped. Retrieve the rest with the adashi_design get_scope operation.]
<!-- adashi:architecture:end -->
