#!/usr/bin/env python3
"""Scripted Claude Code CLI for the one-shot delegation integration test.

The fixture is launched exactly like the installed `claude` binary and asserts
the adapter's argument vector and stdin contract before answering, so a
regression fails the test instead of being tolerated.
`AWORKIT_CLAUDE_ONE_SHOT_MODE` selects the scenario;
`AWORKIT_CLAUDE_FIXTURE_MODEL` / `..._EFFORT` make the fixture require the
matching optional flag; `AWORKIT_CLAUDE_ONE_SHOT_SENTINEL` names the file a
detached descendant writes when it survives process-tree cleanup.
"""
import json
import os
import subprocess
import sys
import time

MODE = os.environ.get("AWORKIT_CLAUDE_ONE_SHOT_MODE", "success")
SENTINEL = os.environ.get("AWORKIT_CLAUDE_ONE_SHOT_SENTINEL")
EXPECTED_MODEL = os.environ.get("AWORKIT_CLAUDE_FIXTURE_MODEL")
EXPECTED_EFFORT = os.environ.get("AWORKIT_CLAUDE_FIXTURE_EFFORT")


def fail(message):
    sys.stderr.write(f"fixture: {message}\n")
    sys.stderr.flush()
    raise SystemExit(3)


def send(value):
    sys.stdout.write(json.dumps(value, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def flag_value(flag):
    if flag not in sys.argv:
        return None
    index = sys.argv.index(flag)
    if index + 1 >= len(sys.argv):
        fail(f"{flag} has no value")
    return sys.argv[index + 1]


for required in ("--print", "--verbose"):
    if required not in sys.argv:
        fail(f"the adapter must pass {required}")
if flag_value("--output-format") != "stream-json":
    fail("the adapter must request stream-json output")
if flag_value("--permission-mode") not in (
    "dontAsk",
    "acceptEdits",
    "auto",
    "plan",
    "bypassPermissions",
):
    fail("the adapter must pass a documented permission mode")
if EXPECTED_MODEL is not None and flag_value("--model") != EXPECTED_MODEL:
    fail(f"expected --model {EXPECTED_MODEL}")
if EXPECTED_EFFORT is not None and flag_value("--effort") != EXPECTED_EFFORT:
    fail(f"expected --effort {EXPECTED_EFFORT}")

task = sys.stdin.read()
if not task.strip():
    fail("the delegated task must arrive on stdin")
if any(task in argument for argument in sys.argv):
    fail("the delegated task must never appear in the argument vector")

send(
    {
        "type": "system",
        "subtype": "init",
        "session_id": "fixture-session",
        "cwd": os.getcwd(),
        "model": "fixture-model",
        "permissionMode": flag_value("--permission-mode"),
    }
)

if MODE == "hang" or MODE == "offspring":
    if MODE == "offspring":
        if not SENTINEL:
            fail("offspring mode requires a sentinel path")
        subprocess.Popen(
            [
                sys.executable,
                "-c",
                "import pathlib,sys,time; time.sleep(1.5); pathlib.Path(sys.argv[1]).write_text('leaked')",
                SENTINEL,
            ]
        )
    time.sleep(60)
    raise SystemExit(0)

send(
    {
        "type": "assistant",
        "parent_tool_use_id": None,
        "message": {
            "model": "fixture-model",
            "content": [{"type": "text", "text": "Working on the delegation"}],
        },
    }
)

if MODE == "denied":
    send(
        {
            "type": "result",
            "subtype": "success",
            "is_error": False,
            "result": "Done under policy",
            "terminal_reason": "completed",
            "permission_denials": [{"tool": "Bash"}, {"tool": "Write"}, {"tool": "Edit"}],
        }
    )
elif MODE == "not-logged-in":
    send(
        {
            "type": "result",
            "subtype": "success",
            "is_error": True,
            "result": "Not logged in \u00b7 Please run /login",
            "terminal_reason": "api_error",
            "api_error_status": None,
            "permission_denials": [],
        }
    )
elif MODE == "silent":
    # A stream that ends without a terminal event at all.
    pass
else:
    send(
        {
            "type": "result",
            "subtype": "success",
            "is_error": False,
            "result": "fixture final answer",
            "terminal_reason": "completed",
            "permission_denials": [],
        }
    )

time.sleep(1)
raise SystemExit(0)
