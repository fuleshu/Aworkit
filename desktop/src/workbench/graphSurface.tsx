import type { JSX } from "react";
import type { JsonObject, WorkflowEditorState } from "./workflow";

export interface WorkflowGraphSurfacePort {
  render(state: WorkflowEditorState, onSelect: (id: string) => void): JSX.Element;
}

/**
 * Replaceable graph-surface adapter. It consumes only editor-kernel JSON and
 * emits selection intents; graph-library-specific state never enters history.
 */
export class WorkflowGraphSurfaceAdapter implements WorkflowGraphSurfacePort {
  public render(state: WorkflowEditorState, onSelect: (id: string) => void): JSX.Element {
    const visibleNodes = state.document.nodes.slice(0, 1000);
    return <div className="graph-surface" aria-label="Workflow graph" role="application">
      <svg aria-hidden="true" className="graph-links" viewBox="0 0 800 500"><path d="M150 150 C300 150 350 300 520 280" /></svg>
      {visibleNodes.map((node, index) => <GraphNode key={nodeId(node, index)} node={node} fallbackIndex={index} selected={state.selectedIds.has(nodeId(node, index))} onSelect={onSelect} />)}
    </div>;
  }
}
function nodeId(node: JsonObject, fallback: number): string { return typeof node.id === "string" ? node.id : `node-${fallback}`; }
function position(node: JsonObject, fallback: number): { left: number; top: number } { const candidate = node.position; if (typeof candidate === "object" && candidate !== null && !Array.isArray(candidate)) { const point = candidate as JsonObject; return { left: typeof point.x === "number" ? point.x : 48 + (fallback % 4) * 160, top: typeof point.y === "number" ? point.y : 48 + Math.floor(fallback / 4) * 120 }; } return { left: 48 + (fallback % 4) * 160, top: 48 + Math.floor(fallback / 4) * 120 }; }
function GraphNode({ node, fallbackIndex, selected, onSelect }: { readonly node: JsonObject; readonly fallbackIndex: number; readonly selected: boolean; readonly onSelect: (id: string) => void }): JSX.Element { const id = nodeId(node, fallbackIndex); const label = typeof node.label === "string" ? node.label : id; return <button aria-pressed={selected} className={`workflow-node${selected ? " is-selected" : ""}`} onClick={() => onSelect(id)} style={position(node, fallbackIndex)} title={`Select workflow node ${label}`} type="button"><span>{label}</span><small>{typeof node.type === "string" ? node.type : "unknown"}</small></button>; }
