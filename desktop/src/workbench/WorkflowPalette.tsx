import { useEffect, useMemo, useState } from "react";
import { NODE_CATALOG } from "./nodeCatalog";
import {
  workflowEdgeId,
  workflowNodeId,
  type WorkflowDocument,
} from "./workflow";

interface WorkflowPaletteProps {
  readonly document: WorkflowDocument;
  readonly editable: boolean;
  readonly selectedIds: ReadonlySet<string>;
  readonly onAddNode: (
    type: string,
    position: { readonly x: number; readonly y: number },
  ) => void;
  readonly onSelect: (id: string) => void;
  readonly onMove: (
    id: string,
    position: { readonly x: number; readonly y: number },
  ) => void;
  readonly onConnect: (source: string, target: string) => void;
  readonly onDelete: (ids: readonly string[]) => void;
}

export function WorkflowPalette({
  document,
  editable,
  selectedIds,
  onAddNode,
  onSelect,
  onMove,
  onConnect,
  onDelete,
}: WorkflowPaletteProps): React.JSX.Element {
  const nodeIds = useMemo(
    () => document.nodes.map((node, index) => workflowNodeId(node, index)),
    [document.nodes],
  );
  const [source, setSource] = useState(nodeIds[0] ?? "");
  const [target, setTarget] = useState(nodeIds[1] ?? nodeIds[0] ?? "");
  const nodeIdKey = nodeIds.join("\u0000");
  useEffect(() => {
    if (!nodeIds.includes(source)) setSource(nodeIds[0] ?? "");
    if (!nodeIds.includes(target))
      setTarget(nodeIds[1] ?? nodeIds[0] ?? "");
  }, [nodeIdKey, nodeIds, source, target]);

  return (
    <aside className="node-palette" aria-label="Workflow toolbox">
      <header>
        <strong>Node toolbox</strong>
        <small>Edits canonical workflow JSON</small>
      </header>
      <p className="workflow-help-copy" role="note">
        {editable
          ? "Add or drag document nodes onto the canvas. Nodes beyond Simple Chat remain savable and exportable but are not labeled executable."
          : "This future-schema workflow is preserved for inspection and lossless export; its graph cannot be edited by this build."}
      </p>
      <section className="node-type-grid" aria-label="Node types">
        <small>NODE TYPES</small>
        {NODE_CATALOG.map(({ type, label, icon, description }) => (
          <button
            aria-label={`Add ${label} node`}
            disabled={!editable}
            draggable={editable}
            key={type}
            title={
              editable
                ? `Add a ${label} node; ${description}`
                : "This workflow schema is inspectable but read-only"
            }
            type="button"
            onClick={() =>
              onAddNode(type, defaultPosition(document.nodes.length))
            }
            onDragStart={(event) => {
              event.dataTransfer.effectAllowed = "copy";
              event.dataTransfer.setData("application/x-aworkit-node", type);
            }}
          >
            <span>{icon}</span>
            <span className="node-palette-label">{label}</span>
          </button>
        ))}
      </section>
      <details className="workflow-outline" open>
        <summary>Document outline</summary>
        <ul aria-label="Workflow nodes">
          {document.nodes.map((node, index) => {
            const id = workflowNodeId(node, index);
            return (
              <li key={id}>
                <button
                  aria-pressed={selectedIds.has(id)}
                  title="Select this node; Alt+Arrow moves it by eight canvas units"
                  type="button"
                  onClick={() => onSelect(id)}
                  onKeyDown={(event) => {
                    if (
                      !editable ||
                      !event.altKey ||
                      !event.key.startsWith("Arrow")
                    )
                      return;
                    event.preventDefault();
                    const point = positionOf(node.position);
                    onMove(id, {
                      x:
                        point.x +
                        (event.key === "ArrowRight"
                          ? 8
                          : event.key === "ArrowLeft"
                            ? -8
                            : 0),
                      y:
                        point.y +
                        (event.key === "ArrowDown"
                          ? 8
                          : event.key === "ArrowUp"
                            ? -8
                            : 0),
                    });
                  }}
                >
                  {String(node.label ?? id)}
                </button>
              </li>
            );
          })}
        </ul>
        <div className="outline-connect">
          <label>
            Transition source
            <select
              disabled={!editable}
              title="Choose the transition source node"
              value={source}
              onChange={(event) => setSource(event.target.value)}
            >
              {nodeOptions(document)}
            </select>
          </label>
          <label>
            Transition target
            <select
              disabled={!editable}
              title="Choose the transition target node"
              value={target}
              onChange={(event) => setTarget(event.target.value)}
            >
              {nodeOptions(document)}
            </select>
          </label>
          <button
            disabled={!editable || source === "" || target === ""}
            title="Create a transition between the selected source and target"
            type="button"
            onClick={() => onConnect(source, target)}
          >
            Add transition
          </button>
        </div>
        <ol aria-label="Workflow transitions">
          {document.edges.map((edge, index) => {
            const id = workflowEdgeId(edge, index);
            return (
              <li className="workflow-edge-row" key={id}>
                <button
                  aria-label={`Select transition ${id}: ${String(edge.source ?? "?")} to ${String(edge.target ?? "?")}`}
                  aria-pressed={selectedIds.has(id)}
                  title="Select this transition"
                  type="button"
                  onClick={() => onSelect(id)}
                >
                  {String(edge.source ?? "?")} → {String(edge.target ?? "?")}
                </button>
                <button
                  aria-label={`Delete transition ${id}`}
                  className="icon-action"
                  disabled={!editable}
                  title="Delete this transition; Undo can restore it"
                  type="button"
                  onClick={() => onDelete([id])}
                >
                  ×
                </button>
              </li>
            );
          })}
        </ol>
      </details>
    </aside>
  );
}

function nodeOptions(document: WorkflowDocument): React.JSX.Element[] {
  return document.nodes.map((node, index) => {
    const id = workflowNodeId(node, index);
    return (
      <option key={id} value={id}>
        {String(node.label ?? id)} ({id})
      </option>
    );
  });
}

function defaultPosition(index: number): { readonly x: number; readonly y: number } {
  return {
    x: 52 + (index % 4) * 190,
    y: 56 + Math.floor(index / 4) * 124,
  };
}

function positionOf(value: unknown): {
  readonly x: number;
  readonly y: number;
} {
  if (typeof value !== "object" || value === null || Array.isArray(value))
    return { x: 0, y: 0 };
  const point = value as Record<string, unknown>;
  return {
    x: typeof point.x === "number" ? point.x : 0,
    y: typeof point.y === "number" ? point.y : 0,
  };
}
