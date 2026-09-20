<!-- adashi:architecture:begin -->
<!-- adashi:generated revision=681 -->
# Architecture — `crates/aworkit-process-platform` (generated)
Generated from the Adashi design model; do not edit, change the model.
These responsibilities are already owned here: extend them, do not duplicate.

- **Invocation Lifecycle & Cross-Platform Process Runtime** (Component) — Responsibilities: Coordinates invocation-local deadlines, cooperative cancellation, forced…

Boundaries crossing this folder:
- Process-Tree Supervisor & Health Monitor -> Invocation Lifecycle & Cross-Platform Process Runtime: Supplies authenticated host/sidecar generation, cancellation and cleanup commands, restart…

Bound here:
- file `crates/aworkit-process-platform/Cargo.toml`

[Showing 1 of 1 design element(s) bound here, 10 further line(s) dropped. Retrieve the rest with the adashi_design get_scope operation.]
<!-- adashi:architecture:end -->
