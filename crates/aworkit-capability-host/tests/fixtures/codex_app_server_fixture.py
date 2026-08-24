#!/usr/bin/env python3
import json
import os
import pathlib
import subprocess
import sys
import time


def send(value):
    sys.stdout.write(json.dumps(value, separators=(",", ":")) + "\n")
    sys.stdout.flush()


mode = os.environ.get("AWORKIT_CODEX_FIXTURE_MODE", "success")
sentinel = os.environ.get("AWORKIT_CODEX_FIXTURE_SENTINEL")
if sentinel:
    subprocess.Popen(
        [
            sys.executable,
            "-c",
            "import pathlib,sys,time; time.sleep(1.5); pathlib.Path(sys.argv[1]).write_text('leaked')",
            sentinel,
        ]
    )

first = sys.stdin.readline()
if mode == "timeout":
    time.sleep(30)
    raise SystemExit(0)
if mode == "nonzero":
    raise SystemExit(23)
if mode == "malformed":
    sys.stdout.write("this is not json\n")
    sys.stdout.flush()
    time.sleep(30)
    raise SystemExit(0)

initialize = json.loads(first)
assert initialize["method"] == "initialize"
assert initialize["params"]["clientInfo"]["name"] == "aworkit"
if mode == "noisy":
    sys.stderr.write("diagnostic-noise" * 65536)
    sys.stderr.flush()
send(
    {
        "id": initialize["id"],
        "result": {
            "userAgent": "codex-fixture/1.0",
            "platformFamily": "fixture",
            "platformOs": "fixture-os",
        },
    }
)

initialized = json.loads(sys.stdin.readline())
assert initialized["method"] == "initialized"
send({"method": "account/updated", "params": {"authMode": "chatgpt"}})

for _ in range(2):
    request = json.loads(sys.stdin.readline())
    if request["method"] == "account/read":
        send(
            {
                "id": request["id"],
                "result": {
                    "account": {
                        "type": "chatgpt",
                        "email": "must-not-cross-the-adapter@example.invalid",
                    },
                    "requiresOpenaiAuth": True,
                },
            }
        )
    elif request["method"] == "model/list":
        send(
            {
                "id": request["id"],
                "result": {
                    "data": [
                        {"id": "model.fixture.one", "displayName": "Fixture One"},
                        {"model": "model.fixture.two", "displayName": "Fixture Two"},
                    ],
                    "nextCursor": None,
                },
            }
        )
    else:
        send(
            {
                "id": request.get("id"),
                "error": {"code": -32601, "message": "unexpected fixture method"},
            }
        )

time.sleep(30)
