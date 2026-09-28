#!/usr/bin/env python3
"""Extract DSH Standard workflow behaviour from the Aworkit chat store.

Read-only: opens ~/.local/share/com.aworkit.desktop/runtime/history/aworkit.sqlite3
and reconstructs, per chat:

  * the plan.1 model_call output (the structured plan),
  * the agent.1 node input,
  * the FIRST model call the agent actually makes, and whether the plan text
    appears anywhere in that call's messages.

This is the decisive check for "does the planner influence the agent?".
"""
from __future__ import annotations

import json
import os
import sqlite3
import sys

DB = os.path.expanduser(
    "~/.local/share/com.aworkit.desktop/runtime/history/aworkit.sqlite3"
)


def load_spans(db, chat_id):
    rows = db.execute(
        "select sequence, kind, payload from semantic_events "
        "where chat_id=? and kind in "
        "('span.started','span.completed','span.failed','span.content_delta') "
        "order by sequence",
        (chat_id,),
    ).fetchall()
    spans = {}
    order = []
    deltas = {}
    for seq, kind, payload in rows:
        d = json.loads(payload)
        sid = d.get("spanId")
        if kind == "span.started":
            spans[sid] = {
                "seq": seq,
                "title": d.get("title"),
                "role": d.get("semanticRole"),
                "node": d.get("nodeId"),
                "parent": d.get("parentSpanId"),
                "input": d.get("input"),
                "output": None,
                "status": "started",
            }
            order.append(sid)
        elif kind == "span.content_delta":
            deltas.setdefault(sid, []).append(d.get("append") or "")
        elif sid in spans:
            spans[sid]["output"] = d.get("output")
            spans[sid]["status"] = "failed" if kind == "span.failed" else "completed"
    for sid, parts in deltas.items():
        if sid in spans:
            spans[sid]["streamed"] = "".join(parts)
    return spans, order


def span_owner_node(spans, sid):
    """Nearest ancestor graph node id for a span, or None."""
    cur = sid
    while cur in spans:
        if spans[cur].get("node"):
            return spans[cur]["node"]
        cur = spans[cur].get("parent")
    return None


def plan_context_stats(spans, order, plan):
    """Split model calls by owning node and count which agent calls carry the
    plan on the per-turn contextMessages channel."""
    frag = str(plan.get("goal") or (plan.get("evidenceNeeded") or [""])[0])[:50]
    agent_calls = []
    plan_calls = 0
    for sid in order:
        s = spans[sid]
        if s.get("role") != "model_call":
            continue
        owner = span_owner_node(spans, sid)
        if owner == "agent.1":
            agent_calls.append(s)
        elif owner == "plan.1":
            plan_calls += 1
    carrying = 0
    first = last = None
    for i, s in enumerate(agent_calls, 1):
        inp = s.get("input")
        cm = inp.get("contextMessages") if isinstance(inp, dict) else None
        if any(frag and frag in (c.get("content") or "") for c in (cm or [])):
            carrying += 1
            if first is None:
                first = i
            last = i
    return {
        "agent_model_calls": len(agent_calls),
        "plan_node_model_calls": plan_calls,
        "agent_calls_carrying_plan": carrying,
        "plan_first_agent_call": first,
        "plan_last_agent_call": last,
    }


def analyze_chat(db, chat_id):
    spans, order = load_spans(db, chat_id)
    by_node = {}
    for sid in order:
        n = spans[sid].get("node")
        if n:
            by_node.setdefault(n, []).append(sid)

    plan_ids = by_node.get("plan.1", [])
    agent_ids = by_node.get("agent.1", [])
    out = {
        "chat_id": chat_id,
        "spans": len(order),
        "plan_node_spans": len(plan_ids),
        "agent_node_spans": len(agent_ids),
    }
    if plan_ids:
        p = spans[plan_ids[0]]
        out["plan_status"] = p["status"]
        out["plan_input_chars"] = len(json.dumps(p.get("input")))
        po = p.get("output")
        out["plan_output_type"] = type(po).__name__
        if isinstance(po, dict):
            out["plan_keys"] = sorted(po.keys())
            out["plan_output_chars"] = len(json.dumps(po))
            out.update(plan_context_stats(spans, order, po))
        elif isinstance(po, str):
            out["plan_output_chars"] = len(po)
    if agent_ids:
        a = spans[agent_ids[0]]
        out["agent_status"] = a["status"]
        out["agent_input_chars"] = len(json.dumps(a.get("input")))
        # The plan reaches the agent on the per-turn contextMessages channel,
        # never inside input.messages. Report the first agent model call's
        # context count so the delivery path is visible in the dump.
        for sid in order:
            s = spans[sid]
            if s.get("role") != "model_call":
                continue
            if span_owner_node(spans, sid) != "agent.1":
                continue
            inp = s.get("input")
            cm = inp.get("contextMessages") if isinstance(inp, dict) else None
            out["agent_first_call_context_messages"] = len(cm or [])
            break
    return out


def main():
    db = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
    rows = db.execute(
        "select chat_id, payload from semantic_events "
        "where kind='chat.started' and payload like '%dsh-standard%' "
        "order by sequence"
    ).fetchall()
    seen = set()
    results = []
    for chat_id, payload in rows:
        if chat_id in seen:
            continue
        seen.add(chat_id)
        d = json.loads(payload)
        if d.get("workflowId") not in ("workflow.dsh-standard",):
            continue
        r = analyze_chat(db, chat_id)
        r["workflow_version"] = d.get("workflowVersion")
        r["created_at"] = d.get("createdAt")
        results.append(r)
    print(json.dumps(results, indent=2))


if __name__ == "__main__":
    main()
