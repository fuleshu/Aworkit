<!-- adashi:architecture:begin -->
<!-- adashi:generated revision=611 -->
# Architecture — `desktop/src-tauri/src/runtime/tool_loop/jobs` (generated)
Generated from the Adashi design model; do not edit, change the model.
These responsibilities are already owned here: extend them, do not duplicate.

- **Capability Invocation Broker** (Component) — Responsibilities: Receives worker capability proposals, obtains the sole core authority de…
- **Process-Tree Supervisor & Health Monitor** (Component) — Responsibilities: Owns lifecycle, identity handshakes, health, cancellation, restart budge…

Boundaries crossing this folder:
- Desktop Command & Event API -> Capability Invocation Broker: Routes approval responses and invocation cancellation by opaque invocation/approval identi…

[Showing 2 of 2 design element(s) bound here, 26 further line(s) dropped. Retrieve the rest with the adashi_design get_scope operation.]
<!-- adashi:architecture:end -->
