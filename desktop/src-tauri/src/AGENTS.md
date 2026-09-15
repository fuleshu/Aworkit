<!-- adashi:architecture:begin -->
<!-- adashi:generated revision=594 -->
# Architecture — `desktop/src-tauri/src` (generated)
Generated from the Adashi design model; do not edit, change the model.
These responsibilities are already owned here: extend them, do not duplicate.

- **Desktop Command & Event API** (Component) — Responsibilities: Implements the only native desktop-facing port, translates schema-versio…

Boundaries crossing this folder:
- Desktop Command & Event API -> Extension Identity, Integrity & Compatibility Registry: Routes inert discovery and explicit install/enable/disable/verify commands and health quer…
- Desktop Command & Event API -> Capability Invocation Broker: Routes approval responses and invocation cancellation by opaque invocation/approval identi…

[Showing 1 of 1 design element(s) bound here, 16 further line(s) dropped. Retrieve the rest with the adashi_design get_scope operation.]
<!-- adashi:architecture:end -->
