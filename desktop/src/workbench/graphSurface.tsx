import {
  Background,
  Controls,
  Handle,
  MiniMap,
  Position,
  ReactFlow,
  ReactFlowProvider,
  useReactFlow,
  type Connection,
  type Edge,
  type Node,
  type NodeProps,
} from "@xyflow/react";
import type { JSX } from "react";
import type { JsonObject, WorkflowEditorState } from "./workflow";
import { workflowEdgeId, workflowNodeId } from "./workflow";

export interface WorkflowGraphCallbacks {
  readonly onSelect: (id: string) => void;
  readonly onClearSelection: () => void;
  readonly onMove: (
    id: string,
    position: { readonly x: number; readonly y: number },
  ) => void;
  readonly onConnect: (
    source: string,
    target: string,
    sourceHandle?: string | null,
    targetHandle?: string | null,
  ) => void;
  readonly onAdd: (
    type: string,
    position: { readonly x: number; readonly y: number },
  ) => void;
}
export interface WorkflowGraphSurfacePort {
  render(
    state: WorkflowEditorState,
    callbacks: WorkflowGraphCallbacks,
  ): JSX.Element;
}

/** React Flow remains a replaceable surface adapter over Aworkit-owned JSON and edit commands. */
export class WorkflowGraphSurfaceAdapter implements WorkflowGraphSurfacePort {
  public render(
    state: WorkflowEditorState,
    callbacks: WorkflowGraphCallbacks,
  ): JSX.Element {
    return (
      <div className="graph-surface" aria-label="Workflow graph">
        <ReactFlowProvider>
          <WorkflowCanvas callbacks={callbacks} state={state} />
        </ReactFlowProvider>
      </div>
    );
  }
}

export interface WorkflowNodeData extends Record<string, unknown> {
  readonly label: string;
  readonly type: string;
  readonly missing: boolean;
  readonly inputPorts: readonly string[];
  readonly outputPorts: readonly string[];
}
function toNode(
  node: JsonObject,
  index: number,
  selected: boolean,
): Node<WorkflowNodeData> {
  const id = workflowNodeId(node, index);
  const point = asObject(node.position);
  return {
    id,
    type: "aworkit",
    position: {
      x: number(point?.x, 48 + (index % 4) * 190),
      y: number(point?.y, 48 + Math.floor(index / 4) * 130),
    },
    data: {
      label: typeof node.label === "string" ? node.label : id,
      type: typeof node.type === "string" ? node.type : "unknown",
      missing: node.capabilityStatus === "missing",
      inputPorts: stringArray(node.inputPorts, "in"),
      outputPorts: stringArray(node.outputPorts, "out"),
    },
    parentId: typeof node.parentId === "string" ? node.parentId : undefined,
    extent: typeof node.parentId === "string" ? "parent" : undefined,
    selected,
    style: {
      width: optionalNumber(node.width),
      height: optionalNumber(node.height),
    },
  } as Node<WorkflowNodeData>;
}
function toEdge(edge: JsonObject, index: number): Edge {
  const source =
    typeof edge.source === "string" ? edge.source : "missing-source";
  const target =
    typeof edge.target === "string" ? edge.target : "missing-target";
  return {
    id: workflowEdgeId(edge, index),
    source,
    target,
    sourceHandle:
      typeof edge.sourcePort === "string" ? edge.sourcePort : undefined,
    targetHandle:
      typeof edge.targetPort === "string" ? edge.targetPort : undefined,
    label: typeof edge.label === "string" ? edge.label : undefined,
    type: source === target ? "smoothstep" : "default",
    animated: edge.active === true,
  };
}
function asObject(value: unknown): JsonObject | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as JsonObject)
    : null;
}
function number(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}
function optionalNumber(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value)
    ? value
    : undefined;
}
function stringArray(value: unknown, fallback: string): readonly string[] {
  return Array.isArray(value) && value.every((item) => typeof item === "string")
    ? value
    : [fallback];
}

function WorkflowCanvas({
  state,
  callbacks,
}: {
  readonly state: WorkflowEditorState;
  readonly callbacks: WorkflowGraphCallbacks;
}): JSX.Element {
  const { screenToFlowPosition } = useReactFlow();
  const { nodes, edges } = projectWorkflowSurface(state);
  return (
    <ReactFlow
      colorMode="system"
      edges={edges}
      fitView
      minZoom={0.15}
      maxZoom={2}
      nodes={nodes}
      nodeTypes={{ aworkit: WorkflowNode }}
      onlyRenderVisibleElements
      onDragOver={(event) => {
        event.preventDefault();
        event.dataTransfer.dropEffect = "copy";
      }}
      onDrop={(event) => {
        event.preventDefault();
        const type = event.dataTransfer.getData("application/x-aworkit-node");
        if (type !== "")
          callbacks.onAdd(
            type,
            screenToFlowPosition({ x: event.clientX, y: event.clientY }),
          );
      }}
      onPaneClick={callbacks.onClearSelection}
      onNodeClick={(_, node) => callbacks.onSelect(node.id)}
      onNodeDragStop={(_, node) => callbacks.onMove(node.id, node.position)}
      onConnect={(connection: Connection) => {
        if (connection.source !== null && connection.target !== null)
          callbacks.onConnect(
            connection.source,
            connection.target,
            connection.sourceHandle,
            connection.targetHandle,
          );
      }}
    >
      <Background color="var(--aw-divider)" gap={16} size={1} />
      <MiniMap
        pannable
        zoomable
        nodeColor={(node) =>
          node.selected ? "var(--aw-accent)" : "var(--aw-control)"
        }
      />
      <Controls showInteractive={false} />
    </ReactFlow>
  );
}

/** Deterministic adapter projection kept testable outside React Flow widget state. */
export function projectWorkflowSurface(state: WorkflowEditorState): {
  readonly nodes: Node<WorkflowNodeData>[];
  readonly edges: Edge[];
} {
  return {
    nodes: state.document.nodes.map((node, index) =>
      toNode(node, index, state.selectedIds.has(workflowNodeId(node, index))),
    ),
    edges: state.document.edges.map(toEdge),
  };
}

function WorkflowNode({
  data,
  selected,
}: NodeProps<Node<WorkflowNodeData>>): JSX.Element {
  return (
    <div
      className={`aworkit-node ${selected ? "selected" : ""} ${data.missing ? "missing" : ""}`}
    >
      {data.inputPorts.map((port, index) => (
        <Handle
          key={port}
          id={port}
          position={Position.Left}
          style={{
            top: `${((index + 1) / (data.inputPorts.length + 1)) * 100}%`,
          }}
          type="target"
        />
      ))}
      <div className="node-icon">{nodeIcon(data.type)}</div>
      <div className="node-copy">
        <strong>{data.label}</strong>
        <span>
          {data.type}
          {data.missing ? " · unresolved" : ""}
        </span>
      </div>
      {data.outputPorts.map((port, index) => (
        <Handle
          key={port}
          id={port}
          position={Position.Right}
          style={{
            top: `${((index + 1) / (data.outputPorts.length + 1)) * 100}%`,
          }}
          type="source"
        />
      ))}
    </div>
  );
}
function nodeIcon(type: string): string {
  if (type.includes("model") || type.includes("agent")) return "AI";
  if (type.includes("tool")) return ">_";
  if (type.includes("input")) return "→";
  if (type.includes("gate")) return "◇";
  return "◆";
}
