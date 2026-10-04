# ComfyUI bridge — reference Aworkit tool plugin

This package is the development reference for an Aworkit tool plugin: a folder
with `tool-plugin.json` plus one MCP server that Aworkit sources, lists and runs.
It is a real plugin, not a mock. It talks to a real ComfyUI server and returns
the real images that server produced.

## Install and enable

1. In Aworkit, open **Settings → Tools → Plugins**.
2. Choose **Open plugin folder**, copy this `comfyui-bridge` folder into it (or
   use **Install plugin…** and select this folder), then choose **Refresh**.
3. The plugin appears as *sourced* and disabled. Choose **Add plugin**, review
   the executable and workflow file on the MCP tab, then **Connect and enable**.
4. To send an API key (only needed when ComfyUI is behind an authenticated
   proxy), add a credential in **Settings → Credentials** and bind it to the
   `COMFYUI_API_KEY` environment variable on the MCP tab.

## Configuration

`bridge.py` is launched with the arguments declared in `tool-plugin.json`. Edit
them on the plugin's own settings after adding it:

| Argument | Meaning |
| --- | --- |
| `--endpoint` | ComfyUI base URL, default `http://127.0.0.1:8188` |
| `--workflows` | JSON file mapping workflow id to an API-format graph, or an inline JSON object |
| `--timeout` | Seconds to wait for one queued workflow, default 120 |
| `--api-key-env` | Optional: name of the environment variable holding the API key, default `COMFYUI_API_KEY` |

`workflows.json` in this folder ships one example. Replace it with the workflow
ids you actually run.

The API key is optional and read from the `COMFYUI_API_KEY` environment variable
by default. To supply one, add a credential in **Settings → Credentials** and
bind it to that variable name on the plugin's settings page. Do not put a key in
the command, its arguments, or the workfflow file, and avoid naming an argument
after a credential (Aworkit refuses an argument that looks like inline
authentication material and asks for a secret-backed environment binding).

## Tools

- `comfyui_status` (read-only) — reachability and configured workflow ids.
- `comfyui_list_workflows` (read-only) — the configured ids.
- `comfyui_run_workflow` — queues one workflow, waits, and returns image
  references. Override node inputs with `<node id>.<input name>` keys.

## Degraded behavior

When ComfyUI is unreachable the tool call returns an MCP error result naming the
endpoint and the fix. The workflow continues; the model is told the plugin is
unavailable and reports it to the user.

## Testing without a GPU

`bridge.py --selftest` runs the tools against the configured endpoint and prints
one JSON result, so a local ComfyUI or a protocol-compatible test server can
validate the package without the desktop application.
