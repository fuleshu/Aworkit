#!/usr/bin/env python3
"""Rename the plan-first workflows and add a faithful DSH-standard workflow.

Target state in the installed Aworkit workflow library:

  workflow.dsh-standard             "Planner Standard"                  (was "DSH Standard")
  workflow.dsh-standard-plan-review "Planner Standard (Plan review)"    (was "DSH Standard (Plan review)")
  workflow.dsh-standard-agent       "DSH Standard"                      (new, no Plan node)

The new document is copied from the installed planner Agent node so it keeps
exactly the tools and compaction policy the profile already has enabled; only
the persona and the graph change.

Writes mirror crates/aworkit-local-store/src/repository.rs::save_document:
content-addressed immutable body plus a manifest CAS entry, under the same
maintenance + collection locks. Read-only on failure paths.
"""
from __future__ import annotations

import fcntl
import hashlib
import importlib.util
import json
import os
import pathlib
import tempfile

HOME = pathlib.Path.home()
STORE = HOME / ".local/share/com.aworkit.desktop/runtime/documents"
WORKFLOWS = STORE / "workflows"
BODIES = WORKFLOWS / "bodies"
MANIFEST = WORKFLOWS / "manifest.json"
MAINTENANCE_LOCK = STORE.parent / ".documents.aworkit-maintenance.lock"
COLLECTION_LOCK = WORKFLOWS / ".repository.lock"
ARTIFACTS = HOME / "aworkit-workflows"
VALIDATOR = ARTIFACTS / "validate_aworkit_workflow.py"

PLANNER_ID = "workflow.dsh-standard"
REVIEW_ID = "workflow.dsh-standard-plan-review"
DSH_ID = "workflow.dsh-standard-agent"

# DSH `standard` preset persona (packages/preset/agent-presets/presets/standard/agent.cordis.yml)
# adapted: the harness identity from dsh-system-prompt, `{{model}}` resolves at
# freeze time in DSH but not in a workflow document, and `{{cwd}}` is the Chat
# project workspace that Aworkit also injects as a project message.
DSH_PERSONA = "\n\n".join([
    "You are an AI agent powered by Aworkit.",
    "You are a coding agent powered by the selected model.",
    "Your working directory is the current Chat project workspace.",
])

PLANNER_COMMENTS = (
    "Planner Standard: plan-first variant of the DeepSeek Harness shipped 'standard' agent "
    "preset. Chat Input -> Plan -> Agent (all configured native tools) -> Chat Output -> Wait "
    "for Input. DSH's plan mode is an opt-in session mode, not a default step; this workflow "
    "instead always runs a tool-free structured Plan model call before the Agent. For the "
    "faithful default DSH standard shape (no plan step) use 'DSH Standard'. The Agent mirrors "
    "DSH's compaction-basic plus tool-result-pruner policy (thresholdChars 81920, headChars "
    "73728, tailChars 4096) so one bounded 64 KiB file read survives pruning intact. Codex and "
    "Claude Code delegation are omitted until an enabled external agent exists in Settings "
    "(DSH disables those rows by default too)."
)

REVIEW_COMMENTS = (
    "Planner Standard (Plan review): the plan-first variant with a plan-review gate: Chat "
    "Input -> Plan -> Approve plan -> Agent (all configured native tools) -> Chat Output -> "
    "Wait for Input. The plan model call's structured plan is shown on the approval card, and "
    "approving lets the agent execute it; rejecting stops the pass. The frozen tool catalog is "
    "identical during planning and execution. For the faithful default DSH standard shape (no "
    "plan step) use 'DSH Standard'."
)

DSH_COMMENTS = (
    "DSH Standard: mirrors the DeepSeek Harness shipped 'standard' agent preset as it behaves "
    "by default - a full coding agent with file editing, shell, file and web search, skills, "
    "goals, subagents and background jobs, and no plan step. Chat Input -> Agent (all "
    "configured native tools) -> Chat Output -> Wait for Input. DSH standard mode does not plan "
    "before acting: it starts work immediately, grounding itself with reads and searches, and "
    "tracks multi-step work with the todo and goal tools. Use 'Planner Standard' when a "
    "plan-first pass is wanted instead. The Agent mirrors DSH's compaction-basic plus "
    "tool-result-pruner policy (thresholdChars 81920, headChars 73728, tailChars 4096) so one "
    "bounded 64 KiB file read survives pruning intact. Codex and Claude Code delegation are "
    "omitted until an enabled external agent exists in Settings (DSH disables those rows by "
    "default too)."
)


def canonical(document: dict) -> bytes:
    """serde_json::to_vec of a Value: sorted keys, compact separators."""
    return json.dumps(document, sort_keys=True, separators=(",", ":")).encode()


def atomic_write(path: pathlib.Path, data: bytes, mode: int) -> None:
    fd, tmp = tempfile.mkstemp(dir=path.parent, prefix=".tmp-")
    try:
        os.fchmod(fd, mode)
        with os.fdopen(fd, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(tmp, path)
    except BaseException:
        try:
            os.unlink(tmp)
        except FileNotFoundError:
            pass
        raise


def load_document(document_id: str, manifest: dict) -> dict:
    entry = manifest["documents"][document_id]
    return json.loads((WORKFLOWS / entry["relative_path"]).read_text())


def agent_node(document: dict) -> dict:
    for node in document["nodes"]:
        if node.get("id") == "agent.1" and node.get("type") == "agent":
            return node
    raise SystemExit("installed planner has no agent.1 node")


def build_dsh_standard(planner: dict) -> dict:
    agent = json.loads(json.dumps(agent_node(planner)))  # deep copy
    agent["position"] = {"x": 245, "y": 205}
    agent["configuration"]["instructions"] = DSH_PERSONA
    return {
        "comments": DSH_COMMENTS,
        "schemaVersion": 1,
        "edges": [
            {"id": "input-agent", "source": "input.1", "target": "agent.1"},
            {"id": "agent-output", "source": "agent.1", "target": "output.1"},
            {"id": "output-wait", "source": "output.1", "target": "wait.1"},
        ],
        "id": DSH_ID,
        "name": "DSH Standard",
        "nodes": [
            {"id": "input.1", "label": "Chat Input", "position": {"x": 36, "y": 205}, "type": "input"},
            agent,
            {"id": "output.1", "label": "Chat Output", "position": {"x": 470, "y": 205}, "type": "output"},
            {"id": "wait.1", "label": "Wait for Input", "position": {"x": 695, "y": 205}, "type": "wait"},
        ],
    }


def install(documents: dict[str, dict]) -> None:
    with open(MAINTENANCE_LOCK, "a+") as maintenance, open(COLLECTION_LOCK, "a+") as collection:
        fcntl.flock(maintenance, fcntl.LOCK_EX)
        fcntl.flock(collection, fcntl.LOCK_EX)

        manifest = json.loads(MANIFEST.read_text())
        if manifest["schema_version"] != 2:
            raise SystemExit(f"unsupported manifest schema {manifest['schema_version']}")
        entries = manifest["documents"]

        for document_id, document in documents.items():
            raw = canonical(document)
            digest = hashlib.sha256(raw).hexdigest()
            relative_path = f"bodies/{digest}.json"
            body_path = WORKFLOWS / relative_path
            previous = entries.get(document_id)
            if previous is not None and previous["content_hash"] == digest:
                print(f"unchanged {document_id} v{previous['document_version']} ({relative_path})")
                continue
            if body_path.exists():
                if body_path.read_bytes() != raw:
                    raise SystemExit(f"body {body_path} exists with different bytes")
            else:
                atomic_write(body_path, raw, 0o600)

            version = 1 if previous is None else int(previous["document_version"]) + 1
            entries[document_id] = {
                "kind": "workflow",
                "document_version": version,
                "schema_version": 1,
                "content_hash": digest,
                "relative_path": relative_path,
                "committed_generation": version,
                "inspectable_read_only": False,
            }
            print(f"installed {document_id} v{version} ({relative_path})")

        manifest["documents"] = dict(sorted(entries.items()))
        atomic_write(MANIFEST, json.dumps(manifest, separators=(",", ":")).encode(), 0o664)

    reloaded = json.loads(MANIFEST.read_text())
    for document_id in documents:
        entry = reloaded["documents"][document_id]
        stored = (WORKFLOWS / entry["relative_path"]).read_bytes()
        assert hashlib.sha256(stored).hexdigest() == entry["content_hash"], document_id
        document = json.loads(stored)
        assert document["id"] == document_id, document_id
        assert document["schemaVersion"] == entry["schema_version"], document_id
        print(f"verified {document_id}: name={document['name']!r} v{entry['document_version']}")


def main() -> int:
    spec = importlib.util.spec_from_file_location("validator", VALIDATOR)
    validator = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(validator)

    manifest = json.loads(MANIFEST.read_text())
    planner = load_document(PLANNER_ID, manifest)
    review = load_document(REVIEW_ID, manifest)

    planner["name"] = "Planner Standard"
    planner["comments"] = PLANNER_COMMENTS
    review["name"] = "Planner Standard (Plan review)"
    review["comments"] = REVIEW_COMMENTS
    dsh = build_dsh_standard(planner)

    documents = {PLANNER_ID: planner, REVIEW_ID: review, DSH_ID: dsh}

    failures = 0
    for document_id, document in documents.items():
        failures += bool(validator.validate(document, document_id))
    # Calibration: the same validator must accept the shipped executable templates.
    bundled = json.loads((pathlib.Path("/home/timofl/src/Aworkit/desktop/workflows/default-workflows.json")).read_text())
    for entry in bundled["workflows"]:
        if entry["seedOnFreshProfile"]:
            failures += bool(validator.validate(entry["document"], f"baseline:{entry['templateId']}"))
    if failures:
        raise SystemExit("validation failed; nothing installed")

    install(documents)

    ARTIFACTS.mkdir(parents=True, exist_ok=True)
    for filename, document_id in [
        ("planner-standard.aworkit.json", PLANNER_ID),
        ("planner-standard-plan-review.aworkit.json", REVIEW_ID),
        ("dsh-standard-agent.aworkit.json", DSH_ID),
    ]:
        path = ARTIFACTS / filename
        path.write_bytes((json.dumps(documents[document_id], indent=2) + "\n").encode())
        print(f"portable copy {path}")

    agent = agent_node(dsh)
    print(f"\nnew DSH Standard: {len(dsh['nodes'])} nodes, "
          f"{len(agent['configuration']['toolIds'])} tools")
    print("instructions:")
    print(agent["configuration"]["instructions"])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
