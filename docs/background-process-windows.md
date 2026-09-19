# Background process windows

Adashi task #100. Owners: Invocation Lifecycle & Cross-Platform Process Runtime;
MCP Client & Session Manager.

## Cause and contract

Shell/Python sessions already used `CREATE_NO_WINDOW`. MCP STDIO used a separate
Tokio launcher without it. Plugin, external-agent probe, worker and native helper
launches used `command-group` defaults, also without console suppression. Setting
flags on their underlying `Command` alone would not fix them: command-group 5.0.1
overwrites those flags when spawning.

All Aworkit-owned background console launches must suppress automatic console
allocation. The shared safe helpers in `aworkit-process::command` apply the flag
to direct commands or the group builder, respectively. Existing pipes, arguments,
environment, containment, cancellation and authority checks remain intact. Unix
behavior is unchanged. This does not hide intentional GUI application windows or
intercept explicit window creation inside third-party programs.

## Production launch audit

| Path | Launch boundary |
| --- | --- |
| Host Shell/Python, jobs and process-backed native tools | Existing `ProcessTree::spawn`, suspended + no-console |
| MCP STDIO discovery, probe, connection and reconnect | `BoundedStdioTransport::spawn`, configured direct command |
| Plugin processes | Shared background group spawn |
| Codex app-server probe | Shared background group spawn |
| Trusted-core workflow worker | Shared background group spawn |
| Native process registry and detached bootstrap helper | Shared background group spawn |
| MCP HTTP and in-process tools | No child process |

## Verification

Use actual Windows console handles from native fixture processes, not screenshots
alone. Cover direct and group launches, real MCP handshake/call/reconnect through
both an executable and a `.cmd` launcher, and native desktop MCP plus Shell/Python
execution in an isolated profile. Verify inherited child launches and keep pipe
output usable. Existing process-session and protocol lifecycle suites cover
cleanup and execution behavior.

Verified on Windows, September 19, 2026:

- 23 integration checks passed across background console probes, MCP STDIO/HTTP,
  plugin protocol, process sessions and Codex app-server probing. The app-server
  fixture now resolves Windows Python so its tests actually execute on Windows.
- The native desktop fixture completed MCP, Shell and Python tool calls. Four
  MCP server starts and every tool probe reported zero root/child console handles.
  Evidence: `desktop/src-tauri/target/native-console-1789805855987/proof.json` and
  `startup.jsonl`; regression: `desktop/scripts/native-console-smoke.mjs`.
- A source audit found no remaining production `group_spawn()` bypasses. The
  managed native process-tree boundary already includes `CREATE_NO_WINDOW`.
