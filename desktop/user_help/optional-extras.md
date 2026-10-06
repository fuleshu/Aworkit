# Example workflows and the FFmpeg plugin

Two extras ship with the Aworkit repository rather than inside the installed
app: four example workflows and a reference tool plugin. Both are optional and
neither is enabled for you.

## Example workflows

The files live in the repository under
[`desktop/workflows/examples`](https://github.com/fuleshu/Aworkit/tree/main/desktop/workflows/examples):

- `triage-router.aworkit.json`
- `evidence-brief.aworkit.json`
- `iterative-planning.aworkit.json`
- `delegated-code-review.aworkit.json`

There is also a plain-language description of each in the folder's
[README](https://github.com/fuleshu/Aworkit/blob/main/desktop/workflows/examples/README.md).

**To install one:**

1. Download the `.aworkit.json` file (or use a local checkout of the
   repository).
2. Open the **Workflows** view in Aworkit.
3. Choose **Import** and select the file.
4. The workflow appears in your library and can be selected in the composer like
   any other.

A brand-new profile already includes **Simple**, **Standard** and **Planer**, so
these four are extras you import when you want them. Only *Delegated Code
Review* has a hard prerequisite — a connected Codex or Claude Code target under
**Settings → External agents** — which is why it is not enabled by default.
More detail: [Workflows](workflows.md).

## FFmpeg media tools plugin

The reference tool plugin lives at
[`desktop/tool-plugins/ffmpeg`](https://github.com/fuleshu/Aworkit/tree/main/desktop/tool-plugins/ffmpeg).
It is a real stdio MCP server (plain Python 3, no third-party packages) that
wraps **FFmpeg** and **FFprobe** as structured tools:

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
`FFPROBE_PATH`, then `PATH`, then the common install folders. On Windows you can
install with `winget install Gyan.FFmpeg` or `scoop install ffmpeg`; on
Debian/Ubuntu use `sudo apt install ffmpeg`; on macOS use `brew install ffmpeg`.

### Install and enable

1. Get the `ffmpeg` folder from the repository link above.
2. In Aworkit, open **Settings → Tool Plugins**.
3. Use **Install plugin…** to select the folder, or **Open plugin folder** and
   copy the folder in, then **Refresh**.
4. Choose **Add plugin**, check the command and arguments on the page (set the
   interpreter to `python3` or an absolute path if `python` is not on `PATH`),
   then **Connect and enable** and **Save configuration**.
5. To pin a specific FFmpeg install, edit the **Arguments** list and put the
   executable path on the line after `--ffmpeg` and after `--ffprobe`.

Full details, including every argument and the tool contract, are in the
plugin's
[README](https://github.com/fuleshu/Aworkit/blob/main/desktop/tool-plugins/ffmpeg/README.md).
Background on the plugin model: [Tool plugins and skills](plugins.md).
