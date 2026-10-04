# FFmpeg media tools — Aworkit tool plugin

This package is an Aworkit tool plugin: a folder with `tool-plugin.json` plus one
real MCP server that wraps FFmpeg and FFprobe as structured tools. It targets
**FFmpeg 9.0.x "Lei"** (current stable) and works from 6.1 up.

The bundled skill in `skills/ffmpeg/SKILL.md` teaches the model when and how to
use the tools, plus the codec, seeking, hardware-acceleration and error guidance
that turns "run ffmpeg" into a correct command.

## Prerequisites

FFmpeg and FFprobe must be installed and on `PATH`, or their absolute paths set
on the plugin's settings page. This package does not ship the binaries.

```sh
# Debian/Ubuntu
sudo apt install ffmpeg
# macOS
brew install ffmpeg
# Windows (winget)
winget install Gyan.FFmpeg
# Windows (scoop)
scoop install ffmpeg
```

The server is plain Python 3 (no third-party packages). The plugin starts it with
`python`; change the command to `python3` or an absolute interpreter path on the
plugin's settings page if `python` is not on `PATH`.

## Install and enable

1. Aworkit → **Settings → Tool Plugins**.
2. **Open plugin folder**, copy this `ffmpeg` folder in (or use **Install plugin…**
   and select it), then **Refresh**.
3. Choose **Add plugin**, check the command and arguments on the page below,
   then **Connect and enable** and **Save configuration**.

## Configuration

Arguments declared in `tool-plugin.json`, editable on the plugin's settings page:

| Argument | Meaning |
| --- | --- |
| `--ffmpeg` | `ffmpeg` executable name or absolute path (default `ffmpeg` from `PATH`) |
| `--ffprobe` | `ffprobe` executable name or absolute path (default `ffprobe`) |
| `--workdir` | Base directory for relative input/output paths (default: the plugin folder) |
| `--timeout` | Per-invocation default in seconds (default 300, maximum 3600) |

Set `--workdir` to the folder your media lives in so relative paths resolve
there; absolute paths always work. No secret is needed: FFmpeg is local, and the
arguments above carry no credential. There is no secret slot to bind.

## Tools

| Tool | What it does | Safety hint |
| --- | --- | --- |
| `ffmpeg_doctor` | versions, resolved paths, available encoders | read-only |
| `ffmpeg_probe` | container, duration, streams, tags via ffprobe | read-only |
| `ffmpeg_convert` | codec/container/resolution/fps/section transcode | input protected |
| `ffmpeg_trim` | cut a segment (fast copy or frame-accurate encode) | input protected |
| `ffmpeg_extract_audio` | mp3/aac/wav/flac/opus/ogg, optional loudnorm | input protected |
| `ffmpeg_thumbnail` | one still frame (png/jpg/webp), scaled | input protected |
| `ffmpeg_gif` | palette-based looping GIF | input protected |
| `ffmpeg_run` | explicit ffmpeg arguments for anything else | may overwrite |

Every structured tool creates a new file, refuses to write over the input, and
refuses to replace an existing output unless `overwrite: true` is passed. Each
returns a `validation` block with a fresh ffprobe summary of the output.

## Degraded behavior

- FFmpeg missing → `ffmpeg_doctor` and every job return an MCP error result that
  names the binary and tells you to install FFmpeg or set `--ffmpeg`/`--ffprobe`.
  The workflow continues; the model reports the problem.
- A job that exceeds its timeout is stopped and reported; a partial output is
  never presented as a success.
- An unsupported codec on this build is refused with the list the bridge knows;
  `ffmpeg_doctor` reports what is actually available.

## Testing

`python3 test_ffmpeg_bridge.py` drives the bridge over real MCP stdio and runs a
real FFmpeg: it generates a small test clip, then probes, converts, trims,
extracts audio, takes a thumbnail, makes a GIF and checks the degraded path with
a missing binary. It skips cleanly if FFmpeg is not installed.

```sh
python3 ffmpeg_bridge.py --selftest   # print one JSON doctor result
```
