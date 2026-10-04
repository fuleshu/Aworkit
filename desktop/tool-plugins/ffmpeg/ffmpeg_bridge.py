#!/usr/bin/env python3
"""Aworkit tool plugin: an FFmpeg bridge.

A real MCP server over standard input/output that exposes FFmpeg and FFprobe as
structured tools. The model coordinates and reviews; FFmpeg does the encoding.
No command is ever built by string concatenation or run through a shell: every
invocation is an explicit argv list.

Targets FFmpeg 9.0.x (current stable, released 2026-09-18) and stays compatible
with 6.1 and newer: it uses only long-stable options and reports the installed
version through `ffmpeg_doctor`.

Configuration (all optional, editable in the plugin's settings):

  --ffmpeg PATH     ffmpeg executable (default: `ffmpeg` from PATH)
  --ffprobe PATH    ffprobe executable (default: `ffprobe` from PATH)
  --workdir PATH    base for relative input/output paths (default: process cwd)
  --timeout SECONDS per-invocation default (default: 300, max 3600)

Diagnostics go to standard error; every byte on standard output is JSON-RPC.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from typing import Any

PROTOCOL_VERSION = "2024-11-05"
SERVER_NAME = "ffmpeg-bridge"
SERVER_VERSION = "1.0.0"
DEFAULT_TIMEOUT = 300.0
MAXIMUM_TIMEOUT = 3600.0
MAX_ARGUMENTS = 256
MAX_ARGUMENT_BYTES = 4096

VIDEO_CODECS = {
    "copy",
    "libx264",
    "libx265",
    "libvpx-vp9",
    "libsvtav1",
    "h264_nvenc",
    "hevc_nvenc",
    "h264_qsv",
    "hevc_qsv",
    "h264_videotoolbox",
    "hevc_videotoolbox",
    "h264_vaapi",
}
AUDIO_CODECS = {
    "copy",
    "aac",
    "libmp3lame",
    "libopus",
    "libvorbis",
    "flac",
    "pcm_s16le",
    "pcm_s24le",
}
PRESETS = {
    "ultrafast",
    "superfast",
    "veryfast",
    "faster",
    "fast",
    "medium",
    "slow",
    "slower",
    "veryslow",
}
FIT_MODES = {"stretch", "pad", "crop"}
AUDIO_FORMATS = {
    "mp3": ("libmp3lame", "mp3"),
    "aac": ("aac", "m4a"),
    "m4a": ("aac", "m4a"),
    "wav": ("pcm_s16le", "wav"),
    "flac": ("flac", "flac"),
    "opus": ("libopus", "opus"),
    "ogg": ("libvorbis", "ogg"),
}
# A per-codec default when the caller does not choose a quality value.
DEFAULT_CRF = {
    "libx264": 23,
    "libx265": 28,
    "libvpx-vp9": 31,
    "libsvtav1": 30,
}


class BridgeError(Exception):
    """A failure reported to the model as an MCP error result, never a crash."""

    def __init__(self, message: str, fix: str | None = None):
        super().__init__(message)
        self.fix = fix


def tool_definitions() -> list[dict]:
    path = {"type": "string", "description": "Media file path (absolute, or relative to the plugin working directory)."}
    overwrite = {"type": "boolean", "description": "Replace the output if it already exists. Default false. The input is never overwritten."}
    timeout = {"type": "integer", "minimum": 5, "maximum": int(MAXIMUM_TIMEOUT), "description": "Seconds before the job is stopped."}
    return [
        {
            "name": "ffmpeg_doctor",
            "description": "Report the installed FFmpeg and FFprobe versions, the configured executable paths, and whether the common encoders (libx264, libx265, AAC, libmp3lame, libopus, libvpx-vp9, libsvtav1, NVENC) are present. Read-only: use it first when FFmpeg may be missing.",
            "inputSchema": {"type": "object", "additionalProperties": False, "properties": {}},
            "annotations": {"readOnlyHint": True, "destructiveHint": False},
        },
        {
            "name": "ffmpeg_probe",
            "description": "Inspect a media file with ffprobe: container, duration, bit rate, and every stream (codec, resolution, frame rate, sample rate, channels) with tags. Read-only.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": False,
                "properties": {
                    "input": path,
                    "include_raw": {"type": "boolean", "description": "Include the complete ffprobe JSON as well as the summary. Default false."},
                },
                "required": ["input"],
            },
            "annotations": {"readOnlyHint": True, "destructiveHint": False},
        },
        {
            "name": "ffmpeg_convert",
            "description": "Transcode one media file to another codec, container, resolution or frame rate. Creates the output; never modifies the input.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": False,
                "properties": {
                    "input": path,
                    "output": path,
                    "video_codec": {"type": "string", "enum": sorted(VIDEO_CODECS), "description": "Video codec. Default libx264. `copy` remuxes the existing video stream."},
                    "audio_codec": {"type": "string", "enum": sorted(AUDIO_CODECS), "description": "Audio codec. Default aac. `copy` remuxes the existing audio stream."},
                    "crf": {"type": "integer", "minimum": 0, "maximum": 63, "description": "Quality for CRF-based encoders (lower is better/larger). Defaults per codec: x264 23, x265 28, VP9 31, AV1 30."},
                    "preset": {"type": "string", "enum": sorted(PRESETS), "description": "Encoder speed/efficiency preset for x264/x265. Default medium."},
                    "video_bitrate": {"type": "string", "description": "Target video bit rate such as `4M`. Replaces CRF when given."},
                    "resolution": {"type": "string", "description": "Target size such as `1920x1080`. Both values must be even for H.264/H.265."},
                    "fit": {"type": "string", "enum": sorted(FIT_MODES), "description": "How to reach the target size: pad (letterbox, default), crop (fill and cut), stretch (distort)."},
                    "fps": {"type": "number", "minimum": 1, "maximum": 240, "description": "Output frame rate."},
                    "audio_bitrate": {"type": "string", "description": "Audio bit rate such as `192k`."},
                    "sample_rate": {"type": "integer", "description": "Output audio sample rate in Hz, for example 48000."},
                    "channels": {"type": "integer", "enum": [1, 2], "description": "Output audio channels: 1 mono, 2 stereo."},
                    "start": {"type": "string", "description": "Optional start time (`HH:MM:SS`, `MM:SS`, or seconds) to convert only a section."},
                    "duration": {"type": "string", "description": "Optional duration to convert, required together with start for a bounded section."},
                    "faststart": {"type": "boolean", "description": "For MP4/MOV, move the index to the front for streaming. Default true."},
                    "strip_metadata": {"type": "boolean", "description": "Drop all metadata instead of copying it from the input. Default false."},
                    "overwrite": overwrite,
                    "timeout_seconds": timeout,
                },
                "required": ["input", "output"],
            },
            "annotations": {"readOnlyHint": False, "destructiveHint": False},
        },
        {
            "name": "ffmpeg_trim",
            "description": "Cut one segment out of a media file. `copy` mode is fast and keyframe-aligned; `encode` mode is frame-accurate. Creates a new file.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": False,
                "properties": {
                    "input": path,
                    "output": path,
                    "start": {"type": "string", "description": "Start time (`HH:MM:SS`, `MM:SS`, or seconds)."},
                    "duration": {"type": "string", "description": "Length to keep. Provide duration or end."},
                    "end": {"type": "string", "description": "End time; the segment ends here. Provide duration or end."},
                    "mode": {"type": "string", "enum": ["copy", "encode"], "description": "copy (default, fast) or encode (frame-accurate)."},
                    "crf": {"type": "integer", "minimum": 0, "maximum": 51, "description": "Quality for encode mode. Default 20."},
                    "overwrite": overwrite,
                    "timeout_seconds": timeout,
                },
                "required": ["input", "output", "start"],
            },
            "annotations": {"readOnlyHint": False, "destructiveHint": False},
        },
        {
            "name": "ffmpeg_extract_audio",
            "description": "Extract or convert the audio track of a media file to mp3, aac/m4a, wav, flac, opus or ogg. Creates a new audio file.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": False,
                "properties": {
                    "input": path,
                    "output": path,
                    "format": {"type": "string", "enum": sorted(AUDIO_FORMATS), "description": "Target audio format. Default mp3."},
                    "bitrate": {"type": "string", "description": "Bit rate for lossy formats such as `192k`. Default 192k."},
                    "sample_rate": {"type": "integer", "description": "Output sample rate in Hz, for example 48000."},
                    "channels": {"type": "integer", "enum": [1, 2], "description": "1 mono, 2 stereo."},
                    "normalize": {"type": "boolean", "description": "Apply EBU R128 loudness normalisation (loudnorm). Default false."},
                    "overwrite": overwrite,
                    "timeout_seconds": timeout,
                },
                "required": ["input", "output"],
            },
            "annotations": {"readOnlyHint": False, "destructiveHint": False},
        },
        {
            "name": "ffmpeg_thumbnail",
            "description": "Write one still image from a video at a chosen timestamp (png, jpg or webp). Read-only with respect to the video; creates the image.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": False,
                "properties": {
                    "input": path,
                    "output": path,
                    "timestamp": {"type": "string", "description": "Where to take the frame (`00:00:05`, `5`, `50%`). Default 00:00:01."},
                    "width": {"type": "integer", "minimum": 0, "maximum": 7680, "description": "Scale to this width, keeping aspect ratio. 0 keeps the source size. Default 0."},
                    "quality": {"type": "integer", "minimum": 1, "maximum": 31, "description": "JPEG/WebP quality scale (lower is better). Default 2."},
                    "overwrite": overwrite,
                    "timeout_seconds": timeout,
                },
                "required": ["input", "output"],
            },
            "annotations": {"readOnlyHint": False, "destructiveHint": False},
        },
        {
            "name": "ffmpeg_gif",
            "description": "Turn a section of a video into a looping GIF using a generated colour palette (two-pass, good quality and small size). Creates the GIF.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": False,
                "properties": {
                    "input": path,
                    "output": path,
                    "start": {"type": "string", "description": "Start time. Default 0."},
                    "duration": {"type": "string", "description": "GIF length. Default 3 seconds."},
                    "fps": {"type": "integer", "minimum": 1, "maximum": 50, "description": "Frames per second. Default 12."},
                    "width": {"type": "integer", "minimum": 16, "maximum": 1920, "description": "Output width in pixels, keeping aspect ratio. Default 480."},
                    "loop": {"type": "boolean", "description": "Loop forever. Default true."},
                    "overwrite": overwrite,
                    "timeout_seconds": timeout,
                },
                "required": ["input", "output"],
            },
            "annotations": {"readOnlyHint": False, "destructiveHint": False},
        },
        {
            "name": "ffmpeg_run",
            "description": "Advanced escape hatch: run ffmpeg with an explicit argument list for an operation no other tool covers. Use the structured tools first. The input is not protected here, so this call can overwrite files.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": False,
                "properties": {
                    "args": {
                        "type": "array",
                        "items": {"type": "string"},
                        "minItems": 1,
                        "maxItems": MAX_ARGUMENTS,
                        "description": "Arguments passed to ffmpeg exactly as given, for example [\"-i\",\"in.mp4\",\"-vf\",\"hflip\",\"out.mp4\"].",
                    },
                    "overwrite": {"type": "boolean", "description": "Add -y so existing outputs are replaced. Default false adds -n and fails instead."},
                    "timeout_seconds": timeout,
                },
                "required": ["args"],
            },
            "annotations": {"readOnlyHint": False, "destructiveHint": True},
        },
    ]


def _text(value: Any) -> str:
    return json.dumps(value, indent=2, ensure_ascii=False, sort_keys=True)


class Bridge:
    def __init__(self, ffmpeg: str, ffprobe: str, workdir: str, timeout: float):
        self.ffmpeg = ffmpeg
        self.ffprobe = ffprobe
        self.workdir = workdir
        self.timeout = timeout

    # ---- process helpers ------------------------------------------------

    def _resolve(self, value: str, *, must_exist: bool) -> str:
        if not isinstance(value, str) or not value.strip():
            raise BridgeError("A file path is required.")
        path = os.path.expanduser(value)
        if not os.path.isabs(path):
            path = os.path.join(self.workdir, path)
        path = os.path.normpath(path)
        if "\0" in path:
            raise BridgeError("A file path contained a NUL character.")
        if must_exist and not os.path.isfile(path):
            raise BridgeError(
                f"Input file not found: {path}",
                "Check the path. Relative paths resolve against the plugin working directory.",
            )
        return path

    def _run(self, executable: str, argv: list[str], timeout: float | None) -> tuple[str, str]:
        command = [executable, *argv]
        # Relative paths inside ffmpeg_run resolve against the configured
        # working directory, matching the structured tools' path resolution.
        working_directory = self.workdir if os.path.isdir(self.workdir) else None
        try:
            completed = subprocess.run(  # noqa: S603 - argv list, never a shell
                command,
                stdin=subprocess.DEVNULL,
                capture_output=True,
                text=True,
                timeout=timeout or self.timeout,
                cwd=working_directory,
            )
        except FileNotFoundError as error:
            raise BridgeError(
                f"{os.path.basename(executable)} was not found ({error}).",
                "Install FFmpeg 9.0.x, or set --ffmpeg and --ffprobe to the executable paths in the plugin settings.",
            ) from error
        except subprocess.TimeoutExpired as error:
            raise BridgeError(
                f"{os.path.basename(executable)} did not finish within {int(timeout or self.timeout)} seconds.",
                "Simplify the job, raise timeout_seconds, or check that the input is not a stalled stream.",
            ) from error
        if completed.returncode != 0:
            tail = "\n".join((completed.stderr or "").strip().splitlines()[-8:])
            raise BridgeError(
                f"{os.path.basename(executable)} exited with status {completed.returncode}.",
                tail or "No diagnostic output was produced.",
            )
        return completed.stdout or "", completed.stderr or ""

    def _guard_output(self, input_path: str, output_path: str, overwrite: bool) -> str:
        output = self._resolve(output_path, must_exist=False)
        if os.path.abspath(output) == os.path.abspath(input_path):
            raise BridgeError(
                "The output would overwrite the input file.",
                "Choose a different output path; the input is never modified.",
            )
        if os.path.exists(output) and not overwrite:
            raise BridgeError(
                f"Output already exists: {output}",
                "Pass overwrite: true or choose another name.",
            )
        parent = os.path.dirname(output)
        if parent and not os.path.isdir(parent):
            raise BridgeError(f"Output folder does not exist: {parent}", "Create it or choose another path.")
        return output

    def _probe_json(self, path: str) -> dict:
        stdout, _ = self._run(
            self.ffprobe,
            ["-v", "error", "-print_format", "json", "-show_format", "-show_streams", path],
            None,
        )
        try:
            return json.loads(stdout or "{}")
        except json.JSONDecodeError as error:
            raise BridgeError("ffprobe did not return valid JSON.", str(error)) from error

    @staticmethod
    def _summarize(probe: dict) -> dict:
        fmt = probe.get("format") or {}
        streams = []
        for stream in probe.get("streams") or []:
            entry = {
                "index": stream.get("index"),
                "type": stream.get("codec_type"),
                "codec": stream.get("codec_name"),
            }
            if stream.get("codec_type") == "video":
                entry["size"] = f"{stream.get('width')}x{stream.get('height')}"
                entry["frame_rate"] = stream.get("r_frame_rate")
                entry["pixel_format"] = stream.get("pix_fmt")
            if stream.get("codec_type") == "audio":
                entry["sample_rate"] = stream.get("sample_rate")
                entry["channels"] = stream.get("channels")
            if stream.get("duration"):
                entry["duration"] = stream.get("duration")
            streams.append(entry)
        summary = {
            "format": fmt.get("format_name"),
            "duration": fmt.get("duration"),
            "size_bytes": _int_or_none(fmt.get("size")),
            "bit_rate": _int_or_none(fmt.get("bit_rate")),
            "streams": streams,
        }
        if fmt.get("tags"):
            summary["tags"] = fmt["tags"]
        return summary

    def _probe_summary(self, path: str) -> dict:
        try:
            return self._summarize(self._probe_json(path))
        except BridgeError as error:
            return {"probe_error": str(error)}

    # ---- basic ffmpeg argv builders ------------------------------------

    def _video_encoding(self, codec: str, arguments: dict) -> list[str]:
        args = ["-c:v", codec]
        if codec == "copy":
            return args
        if codec.startswith(("libx264", "libx265")):
            args += ["-preset", str(arguments.get("preset") or "medium")]
        if arguments.get("video_bitrate"):
            args += ["-b:v", str(arguments["video_bitrate"])]
        elif codec in DEFAULT_CRF or arguments.get("crf") is not None:
            crf = arguments.get("crf")
            if crf is None:
                crf = DEFAULT_CRF.get(codec)
            if crf is not None:
                args += ["-crf", str(int(crf))]
        if codec in {"libx264", "libx265", "h264_nvenc", "hevc_nvenc", "h264_qsv", "hevc_qsv", "h264_videotoolbox", "hevc_videotoolbox"}:
            args += ["-pix_fmt", "yuv420p"]
        return args

    def _video_filters(self, arguments: dict) -> list[str]:
        filters: list[str] = []
        resolution = arguments.get("resolution")
        if resolution:
            width, height = _parse_resolution(resolution)
            fit = arguments.get("fit") or "pad"
            if fit == "stretch":
                filters.append(f"scale={width}:{height}")
            elif fit == "crop":
                filters.append(
                    f"scale={width}:{height}:force_original_aspect_ratio=increase,crop={width}:{height}"
                )
            else:
                filters.append(
                    f"scale={width}:{height}:force_original_aspect_ratio=decrease,"
                    f"pad={width}:{height}:(ow-iw)/2:(oh-ih)/2"
                )
        fps = arguments.get("fps")
        if fps:
            filters.append(f"fps={fps}")
        return ["-vf", ",".join(filters)] if filters else []

    def _audio_encoding(self, codec: str, arguments: dict, *, default_bitrate: bool) -> list[str]:
        args = ["-c:a", codec]
        if codec == "copy":
            return args
        if default_bitrate and codec in {"aac", "libmp3lame", "libopus", "libvorbis"}:
            args += ["-b:a", str(arguments.get("audio_bitrate") or arguments.get("bitrate") or "192k")]
        elif arguments.get("audio_bitrate") or arguments.get("bitrate"):
            args += ["-b:a", str(arguments.get("audio_bitrate") or arguments["bitrate"])]
        return args

    def _container_flags(self, output: str, arguments: dict) -> list[str]:
        args = list(arguments.get("_extra_flags") or [])
        if arguments.get("strip_metadata"):
            args += ["-map_metadata", "-1"]
        else:
            args += ["-map_metadata", "0"]
        extension = os.path.splitext(output)[1].lower()
        if extension in {".mp4", ".m4v", ".mov"} and arguments.get("faststart", True):
            args += ["-movflags", "+faststart"]
        return args

    def _base(self, overwrite: bool) -> list[str]:
        return [self.ffmpeg, "-nostdin", "-hide_banner", "-loglevel", "error", "-y" if overwrite else "-n"]

    # ---- tools ----------------------------------------------------------

    def doctor(self) -> dict:
        version_output, _ = self._run(self.ffmpeg, ["-version"], 30)
        ffmpeg_version = version_output.splitlines()[0] if version_output else "unknown"
        probe_output, _ = self._run(self.ffprobe, ["-version"], 30)
        ffprobe_version = probe_output.splitlines()[0] if probe_output else "unknown"
        encoders_output, _ = self._run(self.ffmpeg, ["-hide_banner", "-encoders"], 30)
        wanted = [
            "libx264",
            "libx265",
            "aac",
            "libmp3lame",
            "libopus",
            "libvpx-vp9",
            "libsvtav1",
            "h264_nvenc",
            "hevc_nvenc",
        ]
        available = sorted(codec for codec in wanted if f" {codec} " in encoders_output)
        missing = sorted(codec for codec in wanted if codec not in available)
        return {
            "ffmpeg": ffmpeg_version,
            "ffprobe": ffprobe_version,
            "ffmpeg_path": shutil.which(self.ffmpeg) or self.ffmpeg,
            "ffprobe_path": shutil.which(self.ffprobe) or self.ffprobe,
            "workdir": self.workdir,
            "encoders_available": available,
            "encoders_missing": missing,
            "notes": "FFmpeg 9.0.x is current; this bridge uses long-stable options and works from 6.1 up.",
        }

    def probe(self, arguments: dict) -> dict:
        path = self._resolve(arguments.get("input", ""), must_exist=True)
        probe = self._probe_json(path)
        result = {"path": path, "media": self._summarize(probe)}
        if arguments.get("include_raw"):
            result["raw"] = probe
        return result

    def convert(self, arguments: dict) -> dict:
        input_path = self._resolve(arguments.get("input", ""), must_exist=True)
        video_codec = str(arguments.get("video_codec") or "libx264")
        audio_codec = str(arguments.get("audio_codec") or "aac")
        if video_codec not in VIDEO_CODECS:
            raise BridgeError(f"Unsupported video codec '{video_codec}'.", f"Choose one of: {', '.join(sorted(VIDEO_CODECS))}.")
        if audio_codec not in AUDIO_CODECS:
            raise BridgeError(f"Unsupported audio codec '{audio_codec}'.", f"Choose one of: {', '.join(sorted(AUDIO_CODECS))}.")
        if arguments.get("resolution"):
            _parse_resolution(arguments["resolution"])
        output = self._guard_output(input_path, arguments.get("output", ""), bool(arguments.get("overwrite")))
        argv = ["-i", input_path]
        if arguments.get("start"):
            argv = ["-ss", str(arguments["start"]), "-i", input_path]
        if arguments.get("duration"):
            if not arguments.get("start"):
                raise BridgeError("`duration` needs `start`.", "Add start, or drop duration to convert the whole file.")
            argv += ["-t", str(arguments["duration"])]
        filters = self._video_filters(arguments)
        if video_codec == "copy" and filters:
            raise BridgeError(
                "Video filters need re-encoding, but video_codec is `copy`.",
                "Use libx264 (or another encoder) when changing resolution or frame rate.",
            )
        argv += filters
        argv += self._video_encoding(video_codec, arguments)
        argv += self._audio_encoding(audio_codec, arguments, default_bitrate=True)
        if arguments.get("sample_rate"):
            argv += ["-ar", str(int(arguments["sample_rate"]))]
        if arguments.get("channels"):
            argv += ["-ac", str(int(arguments["channels"]))]
        argv += self._container_flags(output, arguments)
        argv += [output]
        self._run(*self._invocation(argv, arguments))
        return self._completion("convert", input_path, output, arguments)

    def trim(self, arguments: dict) -> dict:
        input_path = self._resolve(arguments.get("input", ""), must_exist=True)
        start = str(arguments.get("start") or "0")
        duration = arguments.get("duration")
        end = arguments.get("end")
        if duration and end:
            raise BridgeError("Provide either duration or end, not both.")
        if end and not _time_value(end) > _time_value(start):
            raise BridgeError("`end` must be later than `start`.")
        mode = str(arguments.get("mode") or "copy")
        if mode not in {"copy", "encode"}:
            raise BridgeError("`mode` must be copy or encode.")
        output = self._guard_output(input_path, arguments.get("output", ""), bool(arguments.get("overwrite")))
        argv = ["-ss", start, "-i", input_path]
        if duration:
            argv += ["-t", str(duration)]
        elif end:
            # `-ss` before `-i` rebases the output to zero, so `-to` would be
            # measured from the new zero. Convert the end time to a duration.
            duration_seconds = _time_value(str(end)) - _time_value(start)
            if duration_seconds <= 0:
                raise BridgeError("`end` must be later than `start`.")
            argv += ["-t", f"{duration_seconds:.3f}"]
        if mode == "copy":
            argv += ["-c", "copy", "-avoid_negative_ts", "make_zero"]
        else:
            argv += self._video_encoding("libx264", {"crf": arguments.get("crf", 20)})
            argv += self._audio_encoding("aac", {}, default_bitrate=True)
        argv += ["-map_metadata", "0"]
        if os.path.splitext(output)[1].lower() in {".mp4", ".m4v", ".mov"}:
            argv += ["-movflags", "+faststart"]
        argv += [output]
        self._run(*self._invocation(argv, arguments))
        return self._completion("trim", input_path, output, arguments)

    def extract_audio(self, arguments: dict) -> dict:
        input_path = self._resolve(arguments.get("input", ""), must_exist=True)
        audio_format = str(arguments.get("format") or "mp3")
        if audio_format not in AUDIO_FORMATS:
            raise BridgeError(f"Unsupported audio format '{audio_format}'.", f"Choose one of: {', '.join(sorted(AUDIO_FORMATS))}.")
        codec, _ = AUDIO_FORMATS[audio_format]
        output = self._guard_output(input_path, arguments.get("output", ""), bool(arguments.get("overwrite")))
        argv = ["-i", input_path, "-vn", "-c:a", codec]
        if codec in {"aac", "libmp3lame", "libopus", "libvorbis"}:
            argv += ["-b:a", str(arguments.get("bitrate") or "192k")]
        if arguments.get("sample_rate"):
            argv += ["-ar", str(int(arguments["sample_rate"]))]
        if arguments.get("channels"):
            argv += ["-ac", str(int(arguments["channels"]))]
        if arguments.get("normalize"):
            argv += ["-af", "loudnorm=I=-16:TP=-1.5:LRA=11"]
        argv += ["-map_metadata", "0", output]
        self._run(*self._invocation(argv, arguments))
        return self._completion("extract_audio", input_path, output, arguments)

    def thumbnail(self, arguments: dict) -> dict:
        input_path = self._resolve(arguments.get("input", ""), must_exist=True)
        output = self._guard_output(input_path, arguments.get("output", ""), bool(arguments.get("overwrite")))
        timestamp = str(arguments.get("timestamp") or "00:00:01")
        filters = []
        width = int(arguments.get("width") or 0)
        if width > 0:
            filters.append(f"scale={width}:-2")
        argv = ["-ss", timestamp, "-i", input_path, "-frames:v", "1"]
        if filters:
            argv += ["-vf", ",".join(filters)]
        if os.path.splitext(output)[1].lower() in {".jpg", ".jpeg", ".webp"}:
            argv += ["-q:v", str(int(arguments.get("quality") or 2))]
        argv += [output]
        self._run(*self._invocation(argv, arguments))
        return self._completion("thumbnail", input_path, output, arguments)

    def gif(self, arguments: dict) -> dict:
        input_path = self._resolve(arguments.get("input", ""), must_exist=True)
        output = self._guard_output(input_path, arguments.get("output", ""), bool(arguments.get("overwrite")))
        start = str(arguments.get("start") or "0")
        duration = str(arguments.get("duration") or "3")
        fps = int(arguments.get("fps") or 12)
        width = int(arguments.get("width") or 480)
        if width % 2 != 0:
            raise BridgeError("GIF width must be even.", "Use an even width such as 480.")
        palette = output + ".palette.png"
        base = ["-ss", start, "-t", duration, "-i", input_path]
        scale = f"fps={fps},scale={width}:-1:flags=lanczos"
        try:
            self._run(
                *self._invocation(
                    [*base, "-vf", f"{scale},palettegen=stats_mode=diff", "-update", "1", palette],
                    arguments,
                )
            )
            self._run(
                *self._invocation(
                    [
                        *base,
                        "-i",
                        palette,
                        "-lavfi",
                        f"{scale} [x]; [x][1:v] paletteuse=dither=bayer:bayer_scale=3",
                    ]
                    + (["-loop", "0"] if arguments.get("loop", True) else ["-loop", "-1"])
                    + [output],
                    arguments,
                )
            )
        finally:
            if os.path.exists(palette):
                try:
                    os.remove(palette)
                except OSError:
                    pass
        return self._completion("gif", input_path, output, arguments)

    def run(self, arguments: dict) -> dict:
        raw = arguments.get("args")
        if not isinstance(raw, list) or not raw:
            raise BridgeError("`args` must be a non-empty list of strings.")
        if len(raw) > MAX_ARGUMENTS:
            raise BridgeError(f"`args` is limited to {MAX_ARGUMENTS} entries.")
        argv: list[str] = []
        for value in raw:
            if not isinstance(value, str):
                raise BridgeError("Every entry in `args` must be a string.")
            if "\0" in value or len(value) > MAX_ARGUMENT_BYTES:
                raise BridgeError("An argument contained a NUL or exceeded the length bound.")
            argv.append(value)
        overwrite = bool(arguments.get("overwrite"))
        argv = ["-nostdin", "-hide_banner", "-y" if overwrite else "-n", *argv]
        self._run(*self._invocation(argv, arguments))
        return {
            "operation": "run",
            "status": "completed",
            "arguments": raw,
            "overwrite": overwrite,
            "note": "This tool ran ffmpeg directly; no output validation was performed.",
        }

    def _invocation(self, argv: list[str], arguments: dict):
        timeout = _timeout(arguments, self.timeout)
        return self.ffmpeg, argv, timeout

    def _completion(self, operation: str, input_path: str, output: str, arguments: dict) -> dict:
        result = {
            "operation": operation,
            "status": "completed",
            "input": input_path,
            "output": output,
            "size_bytes": os.path.getsize(output) if os.path.isfile(output) else None,
            "validation": self._probe_summary(output),
        }
        return result

    def call(self, name: str, arguments: dict) -> dict:
        if name == "ffmpeg_doctor":
            return self.doctor()
        if name == "ffmpeg_probe":
            return self.probe(arguments)
        if name == "ffmpeg_convert":
            return self.convert(arguments)
        if name == "ffmpeg_trim":
            return self.trim(arguments)
        if name == "ffmpeg_extract_audio":
            return self.extract_audio(arguments)
        if name == "ffmpeg_thumbnail":
            return self.thumbnail(arguments)
        if name == "ffmpeg_gif":
            return self.gif(arguments)
        if name == "ffmpeg_run":
            return self.run(arguments)
        raise BridgeError(f"Unknown tool '{name}'.")


# ---- small helpers ------------------------------------------------------


def _parse_resolution(value: str) -> tuple[int, int]:
    parts = str(value).lower().replace(" ", "").split("x")
    if len(parts) != 2 or not all(part.isdigit() for part in parts):
        raise BridgeError(f"Invalid resolution '{value}'.", "Use WIDTHxHEIGHT such as 1920x1080.")
    width, height = int(parts[0]), int(parts[1])
    if width < 2 or height < 2 or width > 7680 or height > 7680:
        raise BridgeError(f"Resolution '{value}' is outside 2..7680 pixels.")
    if width % 2 or height % 2:
        raise BridgeError(
            f"Resolution '{value}' has an odd dimension.",
            "H.264/H.265 need even width and height; use 1920x1080, not 1921x1080.",
        )
    return width, height


def _time_value(value: str) -> float:
    text = str(value)
    if text.endswith("%"):
        return 0.0
    parts = text.split(":")
    try:
        numbers = [float(part) for part in parts]
    except ValueError:
        return 0.0
    seconds = 0.0
    for number in numbers:
        seconds = seconds * 60 + number
    return seconds


def _int_or_none(value: Any) -> int | None:
    try:
        return int(value)
    except (TypeError, ValueError):
        return None


def _timeout(arguments: dict, default: float) -> float:
    value = arguments.get("timeout_seconds")
    if value is None:
        return default
    seconds = float(value)
    return max(5.0, min(MAXIMUM_TIMEOUT, seconds))


# ---- MCP stdio loop -----------------------------------------------------


def write_message(message: dict) -> None:
    sys.stdout.write(json.dumps(message, separators=(",", ":"), ensure_ascii=False) + "\n")
    sys.stdout.flush()


def result_response(message_id, result: dict) -> dict:
    return {"jsonrpc": "2.0", "id": message_id, "result": result}


def error_response(message_id, code: int, text: str) -> dict:
    return {"jsonrpc": "2.0", "id": message_id, "error": {"code": code, "message": text}}


def tool_result(value: dict | None, text: str, is_error: bool = False) -> dict:
    result: dict = {"content": [{"type": "text", "text": text}], "isError": is_error}
    if value is not None:
        result["structuredContent"] = value
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description="Aworkit FFmpeg bridge (MCP over stdio)")
    parser.add_argument("--ffmpeg", default=os.environ.get("FFMPEG_PATH", "ffmpeg"))
    parser.add_argument("--ffprobe", default=os.environ.get("FFPROBE_PATH", "ffprobe"))
    parser.add_argument("--workdir", default=os.environ.get("FFMPEG_BRIDGE_WORKDIR", os.getcwd()))
    parser.add_argument("--timeout", type=float, default=DEFAULT_TIMEOUT)
    parser.add_argument("--selftest", action="store_true", help="Print one JSON doctor result and exit.")
    args = parser.parse_args()

    bridge = Bridge(
        ffmpeg=args.ffmpeg,
        ffprobe=args.ffprobe,
        workdir=os.path.abspath(os.path.expanduser(args.workdir)),
        timeout=max(5.0, min(MAXIMUM_TIMEOUT, args.timeout)),
    )

    if args.selftest:
        try:
            json.dump(bridge.doctor(), sys.stdout, indent=2, sort_keys=True)
            sys.stdout.write("\n")
            return 0
        except BridgeError as error:
            sys.stderr.write(f"ffmpeg-bridge selftest: {error} {error.fix or ''}\n")
            return 1

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            sys.stderr.write("ffmpeg-bridge: ignored a malformed JSON-RPC line\n")
            continue
        method = message.get("method")
        message_id = message.get("id")
        if method is None or message_id is None:
            continue  # response, or a notification such as notifications/initialized
        if method == "initialize":
            write_message(
                result_response(
                    message_id,
                    {
                        "protocolVersion": PROTOCOL_VERSION,
                        "capabilities": {"tools": {"listChanged": False}},
                        "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
                    },
                )
            )
        elif method in ("ping", "logging/setLevel"):
            write_message(result_response(message_id, {}))
        elif method == "tools/list":
            write_message(result_response(message_id, {"tools": tool_definitions()}))
        elif method == "tools/call":
            params = message.get("params") or {}
            name = params.get("name")
            arguments = params.get("arguments") or {}
            try:
                value = bridge.call(name, arguments if isinstance(arguments, dict) else {})
                write_message(
                    result_response(
                        message_id,
                        tool_result(value, _text(value)),
                    )
                )
            except BridgeError as error:
                text = str(error) + (f"\nFix: {error.fix}" if error.fix else "")
                write_message(result_response(message_id, tool_result(None, text, is_error=True)))
            except Exception as error:  # never expose a traceback to the model
                sys.stderr.write(f"ffmpeg-bridge: tool '{name}' failed: {error}\n")
                write_message(
                    result_response(
                        message_id,
                        tool_result(None, f"The FFmpeg tool call failed: {error}", is_error=True),
                    )
                )
        else:
            # An unknown method is the documented signal that this server is
            # legacy, so a modern client retries with `initialize`.
            write_message(error_response(message_id, -32601, f"Method not found: {method}"))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except BrokenPipeError:
        raise SystemExit(0)
