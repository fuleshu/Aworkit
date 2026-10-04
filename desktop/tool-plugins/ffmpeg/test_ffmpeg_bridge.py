#!/usr/bin/env python3
"""End-to-end check for the FFmpeg tool plugin.

Drives the bridge over real MCP stdio framing and runs a real FFmpeg: it
generates a small test clip, then probes, trims, converts, extracts audio, takes
a thumbnail, makes a GIF, uses the advanced escape hatch, and checks the error
and degraded paths. Skips cleanly when FFmpeg is not installed.

    python3 test_ffmpeg_bridge.py
"""

from __future__ import annotations

import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
BRIDGE = HERE / "ffmpeg_bridge.py"


def call(bridge: subprocess.Popen, message: dict) -> dict:
    assert bridge.stdin is not None and bridge.stdout is not None
    bridge.stdin.write(json.dumps(message) + "\n")
    bridge.stdin.flush()
    line = bridge.stdout.readline()
    if not line:
        stderr = bridge.stderr.read() if bridge.stderr else ""
        raise AssertionError(f"bridge closed its output early\n{stderr}")
    return json.loads(line)


def start_bridge(
    workdir: Path,
    ffmpeg: str = "ffmpeg",
    ffprobe: str = "ffprobe",
    env: dict | None = None,
) -> subprocess.Popen:
    return subprocess.Popen(
        [
            sys.executable,
            str(BRIDGE),
            "--ffmpeg",
            ffmpeg,
            "--ffprobe",
            ffprobe,
            "--workdir",
            str(workdir),
        ],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=env,
    )


def stop(bridge: subprocess.Popen) -> None:
    if bridge.poll() is None:
        bridge.stdin.close()
        bridge.terminate()
        try:
            bridge.wait(timeout=10)
        except subprocess.TimeoutExpired:
            bridge.kill()


def initialize(bridge: subprocess.Popen) -> None:
    reply = call(
        bridge,
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "ffmpeg-test", "version": "1"},
            },
        },
    )
    assert reply["result"]["protocolVersion"] == "2024-11-05", reply


def invoke(bridge: subprocess.Popen, request_id: int, name: str, arguments: dict) -> dict:
    reply = call(
        bridge,
        {
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        },
    )
    return reply["result"]


def generate_sample(path: Path) -> None:
    command = [
        "ffmpeg", "-y", "-nostdin", "-hide_banner", "-loglevel", "error",
        "-f", "lavfi", "-i", "testsrc=size=320x240:rate=10",
        "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
        "-t", "3", "-c:v", "libx264", "-pix_fmt", "yuv420p",
        "-c:a", "aac", "-shortest", str(path),
    ]
    subprocess.run(command, check=True, capture_output=True, text=True)


def load_bridge_module():
    spec = importlib.util.spec_from_file_location("ffmpeg_bridge", BRIDGE)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


def check_resolution_and_paths() -> None:
    """Executable resolution and path cleaning; runs without FFmpeg installed."""
    bridge = load_bridge_module()

    # Windows extended-length prefixes never leak into reported paths.
    assert bridge._clean_path("\\\\?\\C:\\media\\in.mp4") == "C:\\media\\in.mp4"
    assert bridge._clean_path("\\\\?\\UNC\\server\\share\\clip.mp4") == "\\\\server\\share\\clip.mp4"
    assert bridge._clean_path("/plain/posix/path.mp4") == "/plain/posix/path.mp4"

    no_which = lambda name: None  # noqa: E731
    # An explicit path is used exactly as given and attributed to the argument.
    found, source = bridge.resolve_executable(
        str(BRIDGE), env_name="FFMPEG_PATH", default_name="ffmpeg", environ={}, which=no_which
    )
    assert found == bridge._clean_path(str(BRIDGE)), (found, source)
    assert source == "argument", source

    # An explicit path that does not exist fails loudly instead of guessing.
    found, source = bridge.resolve_executable(
        "/nonexistent/ffmpeg", env_name="FFMPEG_PATH", default_name="ffmpeg",
        environ={}, which=no_which,
    )
    assert found is None and source == "argument", (found, source)

    # The environment override wins over a bare default command name.
    found, source = bridge.resolve_executable(
        "ffmpeg", env_name="FFMPEG_PATH", default_name="ffmpeg",
        environ={"FFMPEG_PATH": str(BRIDGE)}, which=no_which,
    )
    assert found == bridge._clean_path(str(BRIDGE)), (found, source)
    assert source == "environment FFMPEG_PATH", source

    # A well-known folder is found even when nothing is on PATH.
    with tempfile.TemporaryDirectory(prefix="aworkit-ffmpeg-resolve-") as folder:
        name = "aworkit-fake-ffmpeg"
        candidate = Path(folder) / (name + (".exe" if os.name == "nt" else ""))
        candidate.write_text("", encoding="utf-8")
        found, source = bridge.resolve_executable(
            name, env_name="FFMPEG_PATH", default_name=name,
            environ={}, which=no_which, extra_dirs=[folder],
        )
        assert found == str(candidate), (found, source)
        assert source.startswith("well-known folder"), source

    # Nothing found anywhere is reported as unresolved, never silently guessed.
    found, source = bridge.resolve_executable(
        "aworkit-no-such-executable", env_name="FFMPEG_PATH",
        default_name="aworkit-no-such-executable", environ={}, which=no_which,
    )
    assert found is None and source == "unresolved", (found, source)
    print("ffmpeg-bridge: resolution checks passed")


def main() -> int:
    check_resolution_and_paths()
    if shutil.which("ffmpeg") is None or shutil.which("ffprobe") is None:
        print("ffmpeg-bridge: skipped (ffmpeg/ffprobe not on PATH)")
        return 0
    encoders = subprocess.run(
        ["ffmpeg", "-hide_banner", "-encoders"], capture_output=True, text=True
    ).stdout
    if " libx264 " not in encoders or " aac " not in encoders:
        print("ffmpeg-bridge: skipped (this build lacks libx264 or aac)")
        return 0

    with tempfile.TemporaryDirectory(prefix="aworkit-ffmpeg-") as folder:
        workdir = Path(folder)
        sample = workdir / "sample.mp4"
        generate_sample(sample)

        bridge = start_bridge(workdir)
        try:
            initialize(bridge)

            catalog = call(
                bridge,
                {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
            )["result"]["tools"]
            names = [tool["name"] for tool in catalog]
            assert names == [
                "ffmpeg_doctor",
                "ffmpeg_probe",
                "ffmpeg_convert",
                "ffmpeg_trim",
                "ffmpeg_extract_audio",
                "ffmpeg_thumbnail",
                "ffmpeg_gif",
                "ffmpeg_run",
            ], names

            doctor = invoke(bridge, 3, "ffmpeg_doctor", {})
            assert doctor["isError"] is False, doctor
            assert "libx264" in doctor["structuredContent"]["encoders_available"], doctor
            # Resolved executable paths are ordinary OS paths, never `\\?\...`.
            assert not doctor["structuredContent"]["ffmpeg_path"].startswith("\\\\?\\"), doctor
            assert not doctor["structuredContent"]["ffprobe_path"].startswith("\\\\?\\"), doctor

            probe = invoke(bridge, 4, "ffmpeg_probe", {"input": "sample.mp4"})
            media = probe["structuredContent"]["media"]
            assert 2.5 <= float(media["duration"]) <= 3.5, media
            video = next(s for s in media["streams"] if s["type"] == "video")
            audio = next(s for s in media["streams"] if s["type"] == "audio")
            assert video["size"] == "320x240", video
            assert audio["sample_rate"] == "48000", audio

            trimmed = invoke(
                bridge, 5, "ffmpeg_trim",
                {"input": "sample.mp4", "output": "trimmed.mp4", "start": "0", "duration": "1.5", "mode": "copy"},
            )
            assert trimmed["isError"] is False, trimmed
            assert Path(trimmed["structuredContent"]["output"]).is_file(), trimmed

            encoded = invoke(
                bridge, 6, "ffmpeg_trim",
                {"input": "sample.mp4", "output": "encoded.mp4", "start": "00:00:01", "duration": "1", "mode": "encode"},
            )
            assert encoded["isError"] is False, encoded

            converted = invoke(
                bridge, 7, "ffmpeg_convert",
                {"input": "sample.mp4", "output": "small.mp4", "video_codec": "libx264",
                 "crf": 30, "preset": "veryfast", "resolution": "160x120", "fit": "pad",
                 "audio_codec": "aac", "audio_bitrate": "96k"},
            )
            assert converted["isError"] is False, converted
            out_video = next(
                s for s in converted["structuredContent"]["validation"]["streams"] if s["type"] == "video"
            )
            assert out_video["size"] == "160x120", converted

            audio = invoke(
                bridge, 8, "ffmpeg_extract_audio",
                {"input": "sample.mp4", "output": "voice.wav", "format": "wav", "channels": 1, "normalize": True},
            )
            assert audio["isError"] is False, audio
            wav = next(
                s for s in audio["structuredContent"]["validation"]["streams"] if s["type"] == "audio"
            )
            assert wav["channels"] == 1, audio

            thumb = invoke(
                bridge, 9, "ffmpeg_thumbnail",
                {"input": "sample.mp4", "output": "cover.jpg", "timestamp": "00:00:01", "width": 160},
            )
            assert thumb["isError"] is False, thumb
            assert Path(thumb["structuredContent"]["output"]).is_file(), thumb

            gif = invoke(
                bridge, 10, "ffmpeg_gif",
                {"input": "sample.mp4", "output": "loop.gif", "start": "0", "duration": "1", "fps": 8, "width": 160},
            )
            assert gif["isError"] is False, gif
            assert Path(gif["structuredContent"]["output"]).stat().st_size > 0, gif

            advanced = invoke(
                bridge, 11, "ffmpeg_run",
                {"args": ["-i", "sample.mp4", "-vf", "hflip", "-c:a", "copy", "flipped.mp4"]},
            )
            assert advanced["isError"] is False, advanced
            assert (workdir / "flipped.mp4").is_file(), advanced

            # The input is protected: an output that equals the input is refused.
            refused = invoke(
                bridge, 12, "ffmpeg_convert",
                {"input": "sample.mp4", "output": "sample.mp4", "video_codec": "libx264"},
            )
            assert refused["isError"] is True, refused
            assert "overwrite the input" in refused["content"][0]["text"].lower(), refused

            missing = invoke(bridge, 13, "ffmpeg_probe", {"input": "does-not-exist.mp4"})
            assert missing["isError"] is True, missing
            assert "not found" in missing["content"][0]["text"].lower(), missing

            # An existing output is not replaced without overwrite.
            again = invoke(
                bridge, 14, "ffmpeg_trim",
                {"input": "sample.mp4", "output": "trimmed.mp4", "start": "0", "duration": "1", "mode": "copy"},
            )
            assert again["isError"] is True, again
            assert "already exists" in again["content"][0]["text"].lower(), again

            # `end` becomes a bounded duration; encode mode proves the precision.
            ranged = invoke(
                bridge, 15, "ffmpeg_trim",
                {"input": "sample.mp4", "output": "ranged.mp4", "start": "00:00:01", "end": "00:00:02", "mode": "encode"},
            )
            assert ranged["isError"] is False, ranged
            ranged_duration = float(ranged["structuredContent"]["validation"]["duration"])
            assert 0.6 <= ranged_duration <= 1.4, ranged
        finally:
            stop(bridge)

        # Degraded path: a missing ffmpeg is a named error, never a crash.
        offline = start_bridge(workdir, ffmpeg="/nonexistent/ffmpeg", ffprobe="/nonexistent/ffprobe")
        try:
            initialize(offline)
            broken = invoke(offline, 1, "ffmpeg_doctor", {})
            assert broken["isError"] is True, broken
            text = broken["content"][0]["text"]
            assert "not found" in text.lower(), broken
            assert "--ffmpeg" in text, broken
        finally:
            stop(offline)

        # FFMPEG_PATH locates FFmpeg even when the command name is wrong.
        located = start_bridge(
            workdir,
            ffmpeg="not-a-real-ffmpeg-name",
            ffprobe="not-a-real-ffprobe-name",
            env={**os.environ, "FFMPEG_PATH": shutil.which("ffmpeg"),
                 "FFPROBE_PATH": shutil.which("ffprobe")},
        )
        try:
            initialize(located)
            found = invoke(located, 1, "ffmpeg_doctor", {})
            assert found["isError"] is False, found
            content = found["structuredContent"]
            assert content["ffmpeg_source"] == "environment FFMPEG_PATH", content
            assert content["ffprobe_source"] == "environment FFPROBE_PATH", content
            assert "not-a-real-ffmpeg-name" not in content["ffmpeg_path"], content
        finally:
            stop(located)

        print("ffmpeg-bridge: all checks passed")
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
