<!-- adashi:architecture:begin -->
<!-- adashi:generated revision=683 -->
# Architecture — `crates/aworkit-capability-host/src/mcp/transport` (generated)
Generated from the Adashi design model; do not edit, change the model.
These responsibilities are already owned here: extend them, do not duplicate.

- **MCP Client & Session Manager** (Component) — Responsibilities: Connects only to configured, core-attested MCP servers and manages initi…

Boundaries crossing this folder:
- External Agent Adapter & Session Manager -> MCP Client & Session Manager: Forwards only the pinned selected MCP server set when both adapter and target negotiated t…

Bound here:
- file `crates/aworkit-capability-host/src/mcp/transport/stdio.rs`

[Showing 1 of 1 design element(s) bound here, 5 further line(s) dropped. Retrieve the rest with the adashi_design get_scope operation.]
<!-- adashi:architecture:end -->
