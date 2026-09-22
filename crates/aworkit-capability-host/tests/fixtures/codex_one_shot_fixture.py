#!/usr/bin/env python3
"""Scripted Codex App Server peer for the one-shot delegation integration test.

The fixture is launched exactly like the real binary (`<fixture> app-server`) and
asserts the client's request shapes before answering, so a protocol regression
fails the test instead of being tolerated. `AWORKIT_CODEX_ONE_SHOT_MODE`
selects the scenario; `AWORKIT_CODEX_ONE_SHOT_SENTINEL` names the file a
detached descendant writes when it survives process-tree cleanup.
"""
import json
import os
import subprocess
import sys
import time

MODE = os.environ.get("AWORKIT_CODEX_ONE_SHOT_MODE", "success")
SENTINEL = os.environ.get("AWORKIT_CODEX_ONE_SHOT_SENTINEL")
THREAD_ID = "thread.fixture"
TURN_ID = "turn.fixture"


def fail(message):
    sys.stderr.write(f"fixture: {message}\n")
    sys.stderr.flush()
    raise SystemExit(3)


def send(value):
    sys.stdout.write(json.dumps(value, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def read_request():
    line = sys.stdin.readline()
    if not line:
        fail("client closed the protocol stream")
    return json.loads(line)


def spawn_descendant():
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


assert sys.argv[1] == "app-server", "the adapter must pass the explicit subcommand"

if MODE == "ambient":
    # The delegated child must see the environment it was launched with, which is
    # how an operator supplies a product API key without storing it in Aworkit.
    if os.environ.get("AWORKIT_FIXTURE_AMBIENT_KEY") != "ambient-present":
        fail("expected AWORKIT_FIXTURE_AMBIENT_KEY from the launching environment")

initialize = read_request()
if initialize["method"] != "initialize":
    fail("expected initialize")
if initialize["params"]["clientInfo"]["name"] != "aworkit":
    fail("expected the Aworkit client identity")
send(
    {
        "id": initialize["id"],
        "result": {
            "userAgent": "codex-one-shot-fixture/1.0",
            "platformFamily": "fixture",
            "platformOs": "fixture-os",
        },
    }
)

if read_request()["method"] != "initialized":
    fail("expected initialized")

thread_start = read_request()
if thread_start["method"] != "thread/start":
    fail("expected thread/start")
if thread_start["params"].get("ephemeral") is not True:
    fail("the run must create an ephemeral thread")
if not os.path.isabs(thread_start["params"].get("cwd", "")):
    fail("thread/start must carry the absolute workspace")
if "approvalPolicy" not in thread_start["params"]:
    fail("thread/start must carry the configured permission mode")
send(
    {
        "id": thread_start["id"],
        "result": {
            "thread": {
                "id": THREAD_ID,
                "ephemeral": MODE != "non-ephemeral",
            }
        },
    }
)

if MODE == "non-ephemeral":
    # The adapter must refuse this thread and never submit a turn.
    line = sys.stdin.readline()
    if line.strip():
        fail("the adapter submitted another request after a non-ephemeral thread")
    raise SystemExit(0)

turn_start = read_request()
if turn_start["method"] != "turn/start":
    fail("expected turn/start")
if turn_start["params"].get("threadId") != THREAD_ID:
    fail("turn/start must reference the created thread")
blocks = turn_start["params"].get("input")
if not isinstance(blocks, list) or len(blocks) != 1:
    fail("turn/start must submit exactly one input block")
if blocks[0].get("type") != "text" or not blocks[0].get("text", "").strip():
    fail("the submitted task must be non-empty text")
if blocks[0].get("text_elements") != []:
    fail("the submitted text block must declare empty text elements")
send({"id": turn_start["id"], "result": {"turn": {"id": TURN_ID}}})

if MODE == "hang" or MODE == "offspring":
    if MODE == "offspring":
        spawn_descendant()
    # Stay silent until the adapter cancels and terminates the process tree.
    time.sleep(60)
    raise SystemExit(0)

if MODE == "failed":
    send(
        {
            "method": "turn/completed",
            "params": {
                "threadId": THREAD_ID,
                "turn": {
                    "id": TURN_ID,
                    "status": "failed",
                    "error": {"codexErrorInfo": "contextWindowExceeded"},
                },
            },
        }
    )
    time.sleep(1)
    raise SystemExit(0)

if MODE == "approval":
    send(
        {
            "id": 91,
            "method": "item/commandExecution/requestApproval",
            "params": {
                "threadId": THREAD_ID,
                "turnId": TURN_ID,
                "command": "rm -rf /",
                "availableDecisions": ["accept", "decline"],
            },
        }
    )
    approval = read_request()
    if approval.get("id") != 91 or approval.get("result") != {"decision": "decline"}:
        fail("the adapter must decline approval unattended")
    send(
        {
            "id": 92,
            "method": "item/tool/requestUserInput",
            "params": {"threadId": THREAD_ID, "turnId": TURN_ID, "questions": []},
        }
    )
    answers = read_request()
    if answers.get("id") != 92 or answers.get("result") != {"answers": {}}:
        fail("the adapter must answer user input as empty unattended")

send(
    {
        "method": "item/completed",
        "params": {
            "threadId": THREAD_ID,
            "turnId": TURN_ID,
            "item": {"type": "agentMessage", "text": "working", "phase": "commentary"},
        },
    }
)
if MODE == "commentary":
    send(
        {
            "method": "turn/completed",
            "params": {"threadId": THREAD_ID, "turn": {"id": TURN_ID, "status": "completed"}},
        }
    )
    time.sleep(1)
    raise SystemExit(0)

send(
    {
        "method": "item/completed",
        "params": {
            "threadId": THREAD_ID,
            "turnId": TURN_ID,
            "item": {
                "type": "agentMessage",
                "text": "fixture final answer",
                "phase": "final_answer",
            },
        },
    }
)
send(
    {
        "method": "turn/completed",
        "params": {"threadId": THREAD_ID, "turn": {"id": TURN_ID, "status": "completed"}},
    }
)
time.sleep(1)
raise SystemExit(0)
