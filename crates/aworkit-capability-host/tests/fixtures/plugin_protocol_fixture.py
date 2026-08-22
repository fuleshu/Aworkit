#!/usr/bin/env python3
"""Language-neutral framed plugin fixture used only by Rust integration tests."""

import json
import os
import struct
import sys


def read_exact(count):
    value = bytearray()
    while len(value) < count:
        chunk = sys.stdin.buffer.read(count - len(value))
        if not chunk:
            return None
        value.extend(chunk)
    return bytes(value)


def read_frame():
    prefix = read_exact(4)
    if prefix is None:
        return None
    size = struct.unpack(">I", prefix)[0]
    if size > 262144:
        raise RuntimeError("fixture received oversized frame")
    body = read_exact(size)
    if body is None:
        raise RuntimeError("fixture received truncated frame")
    return json.loads(body.decode("utf-8"))


def write_frame(message_id, generation, message_type, payload):
    frame = {
        "schemaVersion": 1,
        "messageId": message_id,
        "hostGeneration": generation,
        "message": {"messageType": message_type, "payload": payload},
    }
    body = json.dumps(frame, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    sys.stdout.buffer.write(struct.pack(">I", len(body)))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()


def main():
    generation = None
    while True:
        frame = read_frame()
        if frame is None:
            return 0
        generation = frame["hostGeneration"]
        message = frame["message"]
        kind = message["messageType"]
        payload = message["payload"]
        if kind == "handshake_request":
            write_frame(
                "fixture.handshake",
                generation,
                "handshake_result",
                {"accepted": True, "observed": payload["expected"], "error": None},
            )
        elif kind == "health_request":
            write_frame(
                "fixture.health",
                generation,
                "health_result",
                {"probeId": payload["probeId"], "status": "healthy", "detail": None},
            )
        elif kind == "invocation_request":
            invocation_id = payload["invocationId"]
            if payload["input"].get("crash"):
                sys.stderr.write("fixture: simulated crash after request bytes were accepted\n")
                sys.stderr.flush()
                os._exit(17)
            write_frame(
                "fixture.accepted",
                generation,
                "invocation_accepted",
                {"invocationId": invocation_id},
            )
            write_frame(
                "fixture.progress",
                generation,
                "invocation_event",
                {
                    "invocationId": invocation_id,
                    "sequence": 1,
                    "event": {"kind": "effect_may_have_started"},
                },
            )
            write_frame(
                "fixture.result",
                generation,
                "invocation_result",
                {
                    "invocationId": invocation_id,
                    "status": "succeeded",
                    "effect": "started",
                    "output": {"fixture": "ok"},
                    "error": None,
                },
            )
        elif kind == "cancel_request":
            write_frame(
                "fixture.cancel",
                generation,
                "cancel_result",
                {
                    "invocationId": payload["invocationId"],
                    "confirmed": False,
                    "effect": "unknown",
                },
            )
        elif kind == "shutdown_request":
            write_frame(
                "fixture.shutdown",
                generation,
                "shutdown_result",
                {"clean": True, "detail": None},
            )
            return 0
        else:
            raise RuntimeError(f"unexpected message type: {kind}")


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        sys.stderr.write(f"fixture protocol error: {error}\n")
        sys.stderr.flush()
        raise

