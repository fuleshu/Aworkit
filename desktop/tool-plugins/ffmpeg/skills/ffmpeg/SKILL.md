---
name: ffmpeg
description: Process audio and video with the FFmpeg plugin tools - probe, convert, trim, extract audio, thumbnails, GIFs and advanced ffmpeg arguments, with codec, quality, seeking and hardware-acceleration guidance for FFmpeg 9.0.
---

# FFmpeg

Use this skill for any audio/video transformation: convert, compress, trim, merge,
resize, extract audio, take a still, make a GIF, normalise loudness, burn
subtitles, or remux a container.

The **ffmpeg** plugin wraps the real `ffmpeg`/`ffprobe` binaries as structured
tools. Use the tools instead of writing a command by hand: they validate paths,
protect the input, bound the runtime, and probe the result for you. Reach for
`ffmpeg_run` only for something the structured tools cannot express.

Target: **FFmpeg 9.0.x "Lei"** (current stable, 9.0.2 on 2026-09-18), also
supported on 8.1 "Hoare", 8.0 "Huffman" and 7.1 "Péter". The installed version
is reported by `ffmpeg_doctor`, along with the resolved `ffmpeg`/`ffprobe`
paths and where they came from. If the tools report FFmpeg missing, tell the
user to set `--ffmpeg`/`--ffprobe` to the executable paths in this plugin's
Arguments (Settings → Tool Plugins → FFmpeg media tools) or to set the
`FFMPEG_PATH`/`FFPROBE_PATH` environment variables; installing FFmpeg in a
common folder such as `C:\ffmpeg\bin` or via the package manager also works
without any configuration.

## Workflow

1. **Probe first.** Call `ffmpeg_probe` on the input. Read the real container,
   duration, resolution, frame rate, codecs and audio layout. Do not assume them.
2. **Choose the smallest operation.** Remux (`copy`) beats re-encode; re-encode
   only the stream that must change.
3. **Run one structured tool.** Keep the output path different from the input.
4. **Verify.** Every transform returns a `validation` block (a fresh ffprobe
   summary of the output) plus `size_bytes`. Check the output duration, codec and
   resolution before reporting success. Never claim a result you did not receive.
5. **Report the real paths and numbers** you got back, not the ones you expected.

## Quality and codecs

Prefer CRF over a fixed bit rate: it spends bits where the picture needs them.

| Target | Video | CRF | Audio | Notes |
| --- | --- | --- | --- | --- |
| Maximum compatibility | `libx264` | 18–23 | `aac` 128–192k | MP4, `yuv420p`, `+faststart` |
| Smaller, modern | `libx265` | 24–28 | `aac` or `libopus` | needs a build with libx265 |
| Web / open | `libvpx-vp9` | 30–34 | `libopus` | WebM; slow |
| Best compression | `libsvtav1` | 28–32 | `libopus` | AV1; needs libsvtav1 |
| Lossless audio | `flac` or `pcm_s16le` | — | — | WAV/FLAC |
| Speech | `libopus` 32–64k / `libmp3lame` 64–96k | — | mono | |

- Lower CRF means higher quality and a larger file (0 is nearly lossless).
- `-preset` (ultrafast…veryslow) trades encode time for compression efficiency;
  `medium` is the default, `fast`/`veryfast` for quick jobs, `slow` to archive.
- H.264 in MP4 should be `yuv420p`; the tools add it automatically for libx264,
  libx265 and the hardware encoders.
- H.264/H.265 require **even** width and height. `scale=1280:-2` rounds the
  derived side to an even number; an odd explicit size is rejected.

## Containers

- **MP4/MOV**: broad support; add `+faststart` (default) so it streams before it
  finishes downloading. AAC audio.
- **MKV**: anything inside, including FLAC and multiple subtitle tracks.
- **WebM**: VP9/AV1 video with Opus/Vorbis audio.
- **Remux** (`video_codec: "copy"`, `audio_codec: "copy"`) changes the container
  without re-encoding: fast and lossless, but the codecs must fit the container.
  MP4 cannot hold VP9-with-Opus reliably; use WebM instead.

## Cutting and seeking

- `mode: "copy"` is fast and lossless but lands on the nearest keyframe before
  `start`, so the first fraction of a second can be missing and the cut can shift
  by a few frames.
- `mode: "encode"` re-encodes and is frame-accurate. Use it when the exact frame
  matters (a cut on a beat, a clip that starts on a hard frame).
- When a copy trim is used downstream, a stale first frame or A/V drift usually
  means the cut was not keyframe-aligned. Re-run with `encode`.
- Give times as `HH:MM:SS`, `MM:SS` or plain seconds. To keep everything after a
  point, use `start` with `end` rather than computing a duration.

## Scaling, padding and cropping

`ffmpeg_convert` takes `resolution` (`WIDTHxHEIGHT`) and `fit`:

- `pad` (default) — fit inside the box and add bars; keeps the whole frame.
- `crop` — fill the box and cut the overflow; no bars, loses edges.
- `stretch` — set the exact size and distort; rarely what you want.

Use `pad` when converting landscape to a square or vertical frame, `crop` when
filling a fixed slot. For a one-off filter, `ffmpeg_run` accepts a `-vf` graph.

## Audio

- Extract for playback: `ffmpeg_extract_audio` with `mp3`/`m4a`/`opus`.
- Extract for editing: `wav` (PCM) or `flac` — no generation loss.
- Normalise speech with `normalize: true` (EBU R128 `loudnorm`, −16 LUFS).
- Mono halves the size for speech: `channels: 1`.
- To drop audio, convert with a video-only operation; to drop video, extract audio.

## Subtitles

- **Soft** (toggleable) in MP4: `ffmpeg_run` with `-i in.mp4 -i subs.srt -c copy
  -c:s mov_text out.mp4`. In MKV use `-c:s copy` or `srt`.
- **Burned in** (always visible): `ffmpeg_run` with
  `-i in.mp4 -vf "subtitles=subs.srt" -c:a copy out.mp4`. Needs a build with
  libass; text is re-encoded, so choose a CRF.

## Overlays and watermarks

`ffmpeg_run` with a filter graph, for example a logo in the top-right with 10 px
of margin:

```
-i main.mp4 -i logo.png -filter_complex "[0:v][1:v]overlay=W-w-10:10" -c:a copy out.mp4
```

Add `:format=auto` and `-c:v libx264 -crf 20` if the overlay needs re-encoding.

## Concatenation

- Same codecs and parameters: remux with the concat demuxer —
  `-f concat -safe 0 -i list.txt -c copy out.mp4` where `list.txt` holds
  `file 'a.mp4'` lines. Fast and lossless.
- Different codecs or sizes: use the concat **filter** and re-encode, or convert
  each input to the same profile first, then remux.
- Glitches at the joins usually mean mismatched parameters; normalise the inputs.

## Hardware acceleration

Hardware encoders are much faster, not always smaller or better. Probe with
`ffmpeg_doctor` first: `encoders_missing` tells you what this build supports.

- NVIDIA: `-hwaccel cuda -c:v h264_nvenc` / `hevc_nvenc`. Quality is set with
  `-cq` (the tools fall back to CRF only for software encoders — for NVENC use
  `ffmpeg_run` and pass `-cq 23`).
- Intel Quick Sync: `-hwaccel qsv -c:v h264_qsv`.
- macOS VideoToolbox: `-c:v h264_videotoolbox` (quality with `-q:v`).
- Linux VAAPI: `-vaapi_device /dev/dri/renderD128 -hwaccel vaapi`, plus
  `-vf format=nv12,hwupload` before the VAAPI encoder.
- FFmpeg 8.0+ adds Vulkan-compute codecs (`ffv1_vulkan`) and 9.0 removes some
  deprecated NVENC options; on an older build those flags are unknown, so check
  the version before copying a modern command.

Hardware paths are opaque to the structured tools' quality controls; use them
through `ffmpeg_run`, and verify the output with `ffmpeg_probe`.

## GIFs

`ffmpeg_gif` builds a palette and applies it in two passes, which looks far
better than a direct GIF encode. Keep it short and small: 2–4 seconds, 10–15
fps, 320–480 px wide. A long, large, high-fps GIF can be tens of megabytes.

## Metadata

By default the tools copy source metadata (`-map_metadata 0`). Set
`strip_metadata: true` for a clean deliverable. To read tags, `ffmpeg_probe`
returns the container tags.

## Common errors and fixes

- `moov atom not found` — the MP4 was never finalised (interrupted download or
  recording). Re-obtain the source; re-encoding an incomplete file does not help.
- `height not divisible by 2` / `width not divisible by 2` — use even dimensions
  or `scale=WIDTH:-2`.
- `Unknown encoder 'libx265'` (or `libsvtav1`, `h264_nvenc`) — this build lacks
  it. Run `ffmpeg_doctor` and pick a codec from `encoders_available`.
- `Non-monotonic DTS` — a variable-frame-rate or damaged source. Re-encode with an
  explicit frame rate (`-r 30`) or add `-fflags +genpts`.
- `Output file does not contain any stream` — the mapping dropped everything;
  check that the source actually has the stream you are converting.
- No thumbnail at a timestamp — the time is past the end of the file; probe the
  duration first.
- A copy trim starts on the wrong frame — it is keyframe-aligned; use encode mode.
- `Permission denied` — the output path is not writable; choose a path under the
  workspace.

## Tool reference

| Tool | Use it for | Safety |
| --- | --- | --- |
| `ffmpeg_doctor` | versions, paths, available encoders | read-only |
| `ffmpeg_probe` | container, duration, streams, tags | read-only |
| `ffmpeg_convert` | codec, container, size, fps, section | input protected |
| `ffmpeg_trim` | cut one segment | input protected |
| `ffmpeg_extract_audio` | pull or convert audio | input protected |
| `ffmpeg_thumbnail` | one still frame | input protected |
| `ffmpeg_gif` | short looping GIF | input protected |
| `ffmpeg_run` | anything else | can overwrite |

Every transform creates a new file and refuses to overwrite the input. It also
refuses to replace an existing output unless you pass `overwrite: true`.

### Examples

Probe, then convert to a web-friendly 720p MP4:

```json
{"input": "talk.mov"}
```
```json
{"input": "talk.mov", "output": "talk-720.mp4", "video_codec": "libx264",
 "crf": 22, "preset": "fast", "resolution": "1280x720", "fit": "pad",
 "audio_codec": "aac", "audio_bitrate": "128k", "faststart": true}
```

Trim the first 15 seconds losslessly, then take a still:

```json
{"input": "talk.mov", "output": "intro.mov", "start": "00:00:00", "duration": "15", "mode": "copy"}
```
```json
{"input": "intro.mov", "output": "cover.jpg", "timestamp": "00:00:05", "width": 1280, "quality": 2}
```

Extract normalised mono speech:

```json
{"input": "interview.mp4", "output": "voice.wav", "format": "wav", "channels": 1, "normalize": true}
```

An advanced remux that burns subtitles is a `ffmpeg_run`:

```json
{"args": ["-i", "in.mp4", "-vf", "subtitles=captions.srt", "-c:a", "copy", "out.mp4"], "overwrite": true}
```

## Cost and care

- Media jobs are slow and disk-heavy. Probe first, encode once, and prefer a
  remux when only the container changes.
- Never point the output at the input; the tools reject it, and for good reason.
- Long jobs should use a realistic `timeout_seconds`; a killed encode leaves a
  partial file that `ffmpeg_probe` will report as damaged.
