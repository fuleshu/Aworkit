# Example workflows and the FFmpeg plugin

Every Aworkit installation bundles four example workflows and the reference
FFmpeg plugin. On first run Aworkit copies them into an **Aworkit folder inside
your documents folder**, so they are somewhere you can find, open and edit — not
hidden beside the application.

| System | Folder |
| --- | --- |
| Windows | `Documents\Aworkit` (your real Documents location, even when it is redirected) |
| macOS | `~/Documents/Aworkit` |
| Linux | your documents folder, `~/Documents/Aworkit` by default |

Inside it you will find:

- **`Example Workflows/`** — the four importable workflow documents and their README.
- **`Tool Plugins/ffmpeg/`** — the reference FFmpeg plugin package.

[Open the Aworkit folder](aworkit:documents) — the desktop app reveals it in your
file manager.

If you prefer not to keep a copy there, turn it off under **Settings → Desktop →
Example workflows and FFmpeg plugin**. The same place can write any missing file
again or reveal the folder. Files are only ever created when they are missing, so
anything you edit is kept.

## Example workflows

- `triage-router.aworkit.json`
- `evidence-brief.aworkit.json`
- `iterative-planning.aworkit.json`
- `delegated-code-review.aworkit.json`

**To install one:**

1. Open the **Workflows** view.
2. Choose **Import**.
3. Pick the file from `Example Workflows` in your Aworkit folder.

The workflow appears in your library and can be selected in the composer like any
other. A brand-new profile already includes **Simple**, **Standard** and
**Planer**, so these four are extras you import when you want them.

Only *Delegated Code Review* has a hard prerequisite — a connected Codex or
Claude Code target under **Settings → External agents** — which is why it is not
enabled by default. More detail: [Workflows](workflows.md).

## FFmpeg media tools plugin

`Tool Plugins/ffmpeg` is the reference tool plugin: a real stdio MCP server
(plain Python 3, no third-party packages) that wraps **FFmpeg** and **FFprobe**
as structured tools.

| Tool | What it does |
| --- | --- |
| `ffmpeg_doctor` | versions, resolved paths and available encoders (read-only) |
| `ffmpeg_probe` | container, duration, streams and tags via ffprobe (read-only) |
| `ffmpeg_convert` | codec, container, resolution, frame rate or a section |
| `ffmpeg_trim` | cut a segment — fast keyframe copy or frame-accurate encode |
| `ffmpeg_extract_audio` | mp3, aac/m4a, wav, flac, opus or ogg, with optional loudness normalisation |
| `ffmpeg_thumbnail` | one still frame, scaled |
| `ffmpeg_gif` | a palette-based looping GIF |
| `ffmpeg_run` | an explicit FFmpeg argument list for anything else |

The plugin also bundles a skill that teaches the model when and how to use the
tools, plus the codec, seeking and hardware-acceleration guidance that turns
“run ffmpeg” into a correct command.

### Prerequisites

FFmpeg and FFprobe must be installed, but they do **not** have to be on `PATH`:
the bridge checks the `--ffmpeg` / `--ffprobe` arguments, then `FFMPEG_PATH` /
`FFPROBE_PATH`, then `PATH`, then the common install folders. On Windows use
`winget install Gyan.FFmpeg` or `scoop install ffmpeg`; on Debian/Ubuntu use
`sudo apt install ffmpeg`; on macOS use `brew install ffmpeg`.

### Install and enable

1. Open **Settings → Tool Plugins**.
2. Use **Install plugin…** and pick the `ffmpeg` folder inside
   `Tool Plugins` in your Aworkit documents folder. (Or use **Open plugin
   folder** and copy it in, then **Refresh**.)
3. Choose **Add plugin**, check the command and arguments on the page (set the
   interpreter to `python3` or an absolute path if `python` is not on `PATH`),
   then **Connect and enable** and **Save configuration**.
4. To pin a specific FFmpeg install, edit the **Arguments** list and put the
   executable path on the line after `--ffmpeg` and after `--ffprobe`.

Installing only makes the plugin available; it stays off until you enable it.
Full details, including every argument and the tool contract, are in the
plugin's README (also inside the folder). Background on the model:
[Tool plugins and skills](plugins.md).
