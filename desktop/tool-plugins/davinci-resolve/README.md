# DaVinci Resolve Studio MCP

Blackmagic Design ships a native MCP server with **DaVinci Resolve Studio 21.1 and later**.
`ResolveMCP.exe` is part of the Resolve installation and speaks MCP over standard input and
output, so Aworkit only has to start it — there is no Python package, `PYTHONPATH` or virtual
environment to install.

This plugin packages that bundled server for Aworkit.

## Requirements

- DaVinci Resolve **Studio** 21.1 or later. The free edition does not include the server.
- Resolve running on the same computer as Aworkit.
- Resolve → **Preferences → System → General → External scripting using** set to **Local**.
- **Automatic scripted actions** left at **Allow safe** unless a workflow needs more.
- The MCP server started from Resolve's **File → Setup AI Assistants…** menu is not required;
  Aworkit starts the same executable itself.

## Execution

| Field | Value |
| --- | --- |
| Transport | stdio |
| Command | `C:\Program Files\Blackmagic Design\DaVinci Resolve\ResolveMCP.exe` |
| Arguments | none |

The default installation path is used. If Resolve is installed elsewhere, edit the command in
**Settings → MCP servers → DaVinci Resolve Studio (native MCP)** to point at the real
`ResolveMCP.exe`; on other platforms it is
`/Applications/DaVinci Resolve/DaVinci Resolve.app/Contents/Applications/ResolveMCP` (macOS) or
`/opt/resolve/bin/ResolveMCP` (Linux).

## Install

1. **Settings → Tool Plugins → Refresh**, then **Add plugin** and pick
   `DaVinci Resolve Studio (native MCP)`. A copied plugin is listed but stays disabled until it
   is added here.
2. Enable the server and its tools, then **Discover** to load the live catalog.
3. Bind the tools you trust to the Agent that should use them.

## Tools (18, discovered)

| Tool | Side effect | Purpose |
| --- | --- | --- |
| `get_resolve_status` | read | Is Resolve running, and which version |
| `launch_resolve` | read | Start Resolve, 60 s timeout |
| `get_whats_new` | read | Changelog since a version or date |
| `search_scripting_api` | read | Regex search of the installed API stubs |
| `get_scripting_api` | read | Stub text, outline, selected types, or a file path |
| `get_scripting_docs` | read | Scripting guide, DCTL reference, Fusion tool docs |
| `describe_grade` | read | What a node graph actually contains |
| `get_image` | read | Rendered graded frame, with region of interest |
| `get_thumbnails` | read | Labelled frame grid for a range |
| `list_luts` / `read_dctl` | read | LUT and DCTL inventory |
| `list_extras_packages` | read | Optional AI model packages and status |
| `run_script` | **writes** | Sandboxed Python with the Resolve API |
| `run_script_unsafe` | **writes** | Same, without the sandbox — **disabled by default** |
| `write_dctl` / `generate_lut` | **writes** | Author DCTLs and `.cube` LUTs |
| `delete_lut_file` | **deletes** | Remove a LUT or DCTL |
| `install_extras_package` | **downloads** | Queue package installs |

`run_script` is the main editing tool: most project, timeline, media pool and render work goes
through it after the agent has read the current API. The server deliberately exposes a small,
general surface rather than one tool per API method.

`run_script_unsafe` adds filesystem, network and subprocess access and is seeded disabled.

## Verified

Checked against DaVinci Resolve Studio **21.2.0.6** on Windows on 2026-11-16:

- MCP handshake: protocol `2024-11-05`, server `davinci_resolve` 21.2, 18 tools.
- `get_resolve_status` → `{"running": true, "version": "21.2"}`.
- `run_script` read of the live session → product `DaVinci Resolve Studio`, project open, current
  page read back.

## Notes

- LUTs and DCTLs are written relative to
  `C:\ProgramData\Blackmagic Design\DaVinci Resolve\Support\LUT\MCP` and listed relative to the
  `LUT` folder.
- The server sandboxes `run_script`; a task that needs the filesystem should use an Aworkit file
  tool rather than `run_script_unsafe`.
- Nothing in this plugin is redistributed: it references the executable inside the installed
  Resolve application.
