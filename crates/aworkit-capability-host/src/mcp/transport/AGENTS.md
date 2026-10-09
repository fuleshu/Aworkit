<!-- adashi:architecture:begin -->
<!-- adashi:generated revision=6657281580032122 -->
# Architecture — `crates/aworkit-capability-host/src/mcp/transport` (generated)
Generated from the Adashi design model; do not edit, change the model.
These responsibilities are already owned here: extend them, do not duplicate.

- **MCP Client & Session Manager** (Component) — Responsibilities: Connects only to configured, core-attested MCP servers and manages initi…

Boundaries crossing this folder:
- External Agent Adapter & Session Manager -> MCP Client & Session Manager: Forwards only the pinned selected MCP server set when both adapter and target negotiated t…

Bound here:

Markdown design specifications:


- [MCP session recovery: reconnect instead of requiring an application restart](../../../../../docs/adashi/design-42db389a49359edcd960831556a927367d75f04df00e4586d24baa9f92481a2f.md)
<!-- adashi:architecture:end -->
