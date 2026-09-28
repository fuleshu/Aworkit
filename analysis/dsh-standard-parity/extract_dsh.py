#!/usr/bin/env python3
"""Summarize DSH standard-mode sessions for comparison with the Aworkit
DSH Standard workflow.

Read-only: streams ~/.dsh/sessions/<workspace>/<session>/session.v3.jsonl.zstd
"""
from __future__ import annotations

import collections
import glob
import json
import os
import subprocess
import sys

ROOT = os.path.expanduser(sys.argv[1] if len(sys.argv) > 1 else "~/.dsh/sessions")


def read_session(path):
    out = subprocess.run(["zstd", "-dc", path], capture_output=True).stdout
    lines = [l for l in out.decode("utf8", "replace").splitlines() if l.strip()]
    header = json.loads(lines[0]) if lines else {}
    events = []
    for l in lines[1:]:
        try:
            events.append(json.loads(l))
        except Exception:
            pass
    return header, events


def tool_name(data):
    return data.get("name") or (data.get("message", {}) or {}).get("name") or "?"


def summarize(path):
    header, events = read_session(path)
    counts = collections.Counter(e.get("type") for e in events)
    tools = collections.Counter()
    todo_positions = []
    goal_creates = 0
    plan_mode_system = 0
    planning_prompt_system = 0
    total = len(events)
    for i, e in enumerate(events):
        t = e.get("type")
        if t == "tool/call":
            tools[tool_name(e.get("data", {}))] += 1
            if tool_name(e.get("data", {})) == "todo_write":
                todo_positions.append(i)
        if t == "goal/change" and e["data"].get("operation") == "create":
            goal_creates += 1
        if t == "system/message":
            txt = json.dumps(e.get("data", {}))
            if "You are in plan mode" in txt:
                plan_mode_system += 1
            if "You are planning the request" in txt:
                planning_prompt_system += 1
    return {
        "session": os.path.basename(os.path.dirname(path)),
        "preset": header.get("agentPreset"),
        "events": total,
        "user_msgs": counts["user/message"],
        "assistant_msgs": counts["assistant/message"],
        "tool_calls": sum(tools.values()),
        "todo_write": tools["todo_write"],
        "todo_positions_pct": [round(100 * p / max(1, total)) for p in todo_positions],
        "goal_creates": goal_creates,
        "plan_mode_system_messages": plan_mode_system,
        "planning_prompt_system_messages": planning_prompt_system,
        "top_tools": tools.most_common(8),
    }


def main():
    paths = sorted(glob.glob(os.path.join(ROOT, "*", "*", "session.v3.jsonl.zstd")))
    results = [summarize(p) for p in paths]
    print(json.dumps(results, indent=2))


if __name__ == "__main__":
    main()
