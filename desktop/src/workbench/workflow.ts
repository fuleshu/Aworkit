/** Framework-independent, lossless workflow-document editor kernel. */
import {
  catalogEntryForType,
  portKindsConnect,
  resolveInputPortKind,
  resolveOutputPortKind,
} from "./nodeCatalog";

export type JsonValue =
  | null
  | boolean
  | number
  | string
  | readonly JsonValue[]
  | { readonly [key: string]: JsonValue };
export type JsonObject = { [key: string]: JsonValue };
export interface WorkflowDocument {
  readonly schemaVersion: number;
  readonly nodes: readonly JsonObject[];
  readonly edges: readonly JsonObject[];
  readonly [key: string]: JsonValue;
}
export interface WorkflowEditorState {
  readonly document: WorkflowDocument;
  readonly selectedIds: ReadonlySet<string>;
  readonly undo: readonly WorkflowDocument[];
  readonly redo: readonly WorkflowDocument[];
  readonly revision: number;
}
export interface WorkflowValidationIssue {
  readonly code:
    | "unsupported_schema"
    | "duplicate_node"
    | "duplicate_edge"
    | "missing_source"
    | "missing_target"
    | "invalid_node"
    | "invalid_edge"
    | "invalid_configuration"
    | "missing_dependency"
    | "unknown_port"
    | "connection_type_mismatch"
    | "condition_route_missing"
    | "cycle_detected";
  readonly itemId: string;
  readonly message: string;
}
export interface WorkflowSummary {
  readonly nodes: number;
  readonly edges: number;
  readonly unresolved: number;
  readonly issues: number;
}
export interface WorkflowPropertySchema {
  readonly key: string;
  readonly label: string;
  readonly type: "string" | "number" | "boolean";
  readonly required?: boolean;
}
export interface WorkflowPropertyDraft {
  readonly nodeId: string;
  readonly values: Readonly<Record<string, JsonValue>>;
  readonly errors: Readonly<Record<string, string>>;
}

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}
function isJsonObject(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
function asDocument(value: unknown): WorkflowDocument {
  if (typeof value !== "object" || value === null || Array.isArray(value))
    throw new Error("workflow must be a JSON object");
  const document = clone(value as WorkflowDocument);
  if (
    !Number.isInteger(document.schemaVersion) ||
    document.schemaVersion < 1 ||
    !Array.isArray(document.nodes) ||
    !Array.isArray(document.edges) ||
    !document.nodes.every(isJsonObject) ||
    !document.edges.every(isJsonObject)
  )
    throw new Error(
      "workflow requires a positive schemaVersion plus arrays of node and transition objects",
    );
  return document;
}
export function parseWorkflow(raw: string): WorkflowDocument {
  return asDocument(JSON.parse(raw));
}
export function serializeWorkflow(document: WorkflowDocument): string {
  return JSON.stringify(asDocument(document));
}
export function createEditor(document: WorkflowDocument): WorkflowEditorState {
  return {
    document: asDocument(document),
    selectedIds: new Set(),
    undo: [],
    redo: [],
    revision: 0,
  };
}

/** Applies one atomic typed edit while preserving every unknown JSON field. */
export function editWorkflow(
  state: WorkflowEditorState,
  mutate: (document: WorkflowDocument) => WorkflowDocument,
): WorkflowEditorState {
  const document = mutate(state.document);
  if (document === state.document) return state;
  if (
    !Number.isInteger(document.schemaVersion) ||
    document.schemaVersion < 1 ||
    !Array.isArray(document.nodes) ||
    !Array.isArray(document.edges)
  ) {
    throw new Error("workflow edit produced an invalid document shape");
  }
  return {
    ...state,
    document,
    undo: [...state.undo, state.document],
    redo: [],
    revision: state.revision + 1,
  };
}

/** Coalesces any number of pure document operations into one undo boundary. */
export function coalesceWorkflowEdits(
  state: WorkflowEditorState,
  operations: readonly ((document: WorkflowDocument) => WorkflowDocument)[],
): WorkflowEditorState {
  return editWorkflow(state, (document) =>
    operations.reduce((current, operation) => operation(current), document),
  );
}
export function undoWorkflow(state: WorkflowEditorState): WorkflowEditorState {
  const previous = state.undo.at(-1);
  return previous === undefined
    ? state
    : {
        ...state,
        document: previous,
        undo: state.undo.slice(0, -1),
        redo: [state.document, ...state.redo],
        revision: state.revision + 1,
      };
}
export function redoWorkflow(state: WorkflowEditorState): WorkflowEditorState {
  const next = state.redo[0];
  return next === undefined
    ? state
    : {
        ...state,
        document: next,
        undo: [...state.undo, state.document],
        redo: state.redo.slice(1),
        revision: state.revision + 1,
      };
}
export function selectWorkflowNode(
  state: WorkflowEditorState,
  nodeId: string,
): WorkflowEditorState {
  return { ...state, selectedIds: new Set([nodeId]) };
}
export function selectWorkflowItem(
  state: WorkflowEditorState,
  itemId: string,
): WorkflowEditorState {
  return { ...state, selectedIds: new Set([itemId]) };
}
export function clearWorkflowSelection(
  state: WorkflowEditorState,
): WorkflowEditorState {
  return { ...state, selectedIds: new Set() };
}

export function addWorkflowNode(
  state: WorkflowEditorState,
  type: string,
  position: { readonly x: number; readonly y: number },
): WorkflowEditorState {
  const existing = new Set(
    state.document.nodes.map((node, index) => nodeId(node, index)),
  );
  const idPrefix = type.trim().replace(/[^a-zA-Z0-9_-]+/g, "-") || "node";
  let counter = state.document.nodes.length + 1;
  let id = `${idPrefix}.${counter}`;
  while (existing.has(id)) id = `${idPrefix}.${++counter}`;
  const edited = editWorkflow(state, (document) => ({
    ...document,
    nodes: [
      ...document.nodes,
      {
        id,
        type,
        label: labelFor(type),
        position: { x: position.x, y: position.y },
        configuration: defaultConfigurationFor(type),
      },
    ],
  }));
  return selectWorkflowItem(edited, id);
}
export function moveWorkflowNode(
  state: WorkflowEditorState,
  id: string,
  position: { readonly x: number; readonly y: number },
): WorkflowEditorState {
  return editWorkflow(state, (document) => ({
    ...document,
    nodes: document.nodes.map((node, index) =>
      nodeId(node, index) === id
        ? { ...node, position: { x: position.x, y: position.y } }
        : node,
    ),
  }));
}
export function connectWorkflowNodes(
  state: WorkflowEditorState,
  source: string,
  target: string,
  sourceHandle?: string | null,
  targetHandle?: string | null,
): WorkflowEditorState {
  const edgeId = uniqueEdgeId(state.document, source, target);
  const edited = editWorkflow(state, (document) => ({
    ...document,
    edges: [
      ...document.edges,
      {
        id: edgeId,
        source,
        target,
        sourcePort: sourceHandle ?? "out",
        targetPort: targetHandle ?? "in",
      },
    ],
  }));
  return selectWorkflowItem(edited, edgeId);
}
export function deleteSelectedWorkflowItems(
  state: WorkflowEditorState,
): WorkflowEditorState {
  return deleteWorkflowItems(state, state.selectedIds);
}
export function deleteWorkflowItems(
  state: WorkflowEditorState,
  itemIds: ReadonlySet<string>,
): WorkflowEditorState {
  if (itemIds.size === 0) return state;
  const remainingNodes = state.document.nodes.filter(
    (node, index) => !itemIds.has(nodeId(node, index)),
  );
  const remainingEdges = state.document.edges.filter((edge, index) => {
    const id = edgeId(edge, index);
    return (
      !itemIds.has(id) &&
      !(typeof edge.source === "string" && itemIds.has(edge.source)) &&
      !(typeof edge.target === "string" && itemIds.has(edge.target))
    );
  });
  if (
    remainingNodes.length === state.document.nodes.length &&
    remainingEdges.length === state.document.edges.length
  )
    return state;
  const edited = editWorkflow(state, (document) => ({
    ...document,
    nodes: remainingNodes,
    edges: remainingEdges,
  }));
  return { ...edited, selectedIds: new Set() };
}
export function updateSelectedNodeProperty(
  state: WorkflowEditorState,
  property: "label" | "type",
  value: string,
): WorkflowEditorState {
  const selected = state.selectedIds.values().next().value as
    | string
    | undefined;
  if (selected === undefined) return state;
  return editWorkflow(state, (document) => ({
    ...document,
    nodes: document.nodes.map((node, index) =>
      nodeId(node, index) === selected ? { ...node, [property]: value } : node,
    ),
  }));
}

export function updateSelectedNodeField(
  state: WorkflowEditorState,
  property: string,
  value: JsonValue,
): WorkflowEditorState {
  return updateSelectedNodeFields(state, { [property]: value });
}

export function updateSelectedNodeFields(
  state: WorkflowEditorState,
  patch: JsonObject,
): WorkflowEditorState {
  const selected = state.selectedIds.values().next().value as
    | string
    | undefined;
  if (selected === undefined || Object.keys(patch).length === 0) return state;
  return editWorkflow(state, (document) => ({
    ...document,
    nodes: document.nodes.map((node, index) =>
      nodeId(node, index) === selected ? { ...node, ...patch } : node,
    ),
  }));
}

export function renameWorkflowNode(
  state: WorkflowEditorState,
  currentId: string,
  nextId: string,
): WorkflowEditorState {
  if (currentId === nextId) return state;
  const edited = editWorkflow(state, (document) => ({
    ...document,
    nodes: document.nodes.map((node, index) =>
      nodeId(node, index) === currentId ? { ...node, id: nextId } : node,
    ),
    edges: document.edges.map((edge) => ({
      ...edge,
      ...(edge.source === currentId ? { source: nextId } : {}),
      ...(edge.target === currentId ? { target: nextId } : {}),
    })),
  }));
  return selectWorkflowItem(edited, nextId);
}

export function updateSelectedEdgeFields(
  state: WorkflowEditorState,
  patch: JsonObject,
): WorkflowEditorState {
  const selected = state.selectedIds.values().next().value as
    | string
    | undefined;
  if (selected === undefined || Object.keys(patch).length === 0) return state;
  return editWorkflow(state, (document) => ({
    ...document,
    edges: document.edges.map((edge, index) =>
      edgeId(edge, index) === selected ? { ...edge, ...patch } : edge,
    ),
  }));
}

/** Merges typed fields into the selected node's `configuration` object. */
export function updateSelectedNodeConfiguration(
  state: WorkflowEditorState,
  patch: JsonObject,
): WorkflowEditorState {
  const selected = state.selectedIds.values().next().value as
    | string
    | undefined;
  if (selected === undefined || Object.keys(patch).length === 0) return state;
  return editWorkflow(state, (document) => ({
    ...document,
    nodes: document.nodes.map((node, index) => {
      if (nodeId(node, index) !== selected) return node;
      const existing = asConfigurationObject(node.configuration);
      return { ...node, configuration: { ...existing, ...patch } };
    }),
  }));
}

function asConfigurationObject(value: unknown): JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as JsonObject)
    : {};
}

export function replaceWorkflowDocument(
  state: WorkflowEditorState,
  document: WorkflowDocument,
): WorkflowEditorState {
  const edited = editWorkflow(state, () => asDocument(document));
  return { ...edited, selectedIds: new Set() };
}

export function createPropertyDraft(
  state: WorkflowEditorState,
  nodeId: string,
  schema: readonly WorkflowPropertySchema[],
): WorkflowPropertyDraft {
  const node = state.document.nodes.find(
    (candidate, index) => workflowNodeId(candidate, index) === nodeId,
  );
  if (node === undefined)
    throw new Error(`workflow node ${nodeId} does not exist`);
  return {
    nodeId,
    values: Object.fromEntries(
      schema.map((field) => [field.key, node[field.key] ?? null]),
    ),
    errors: {},
  };
}

export function updatePropertyDraft(
  draft: WorkflowPropertyDraft,
  schema: readonly WorkflowPropertySchema[],
  key: string,
  input: string | boolean,
): WorkflowPropertyDraft {
  const field = schema.find((candidate) => candidate.key === key);
  if (field === undefined) throw new Error(`unknown workflow property ${key}`);
  let value: JsonValue = input;
  let error: string | undefined;
  if (field.type === "number") {
    const parsed = typeof input === "string" ? Number(input) : Number.NaN;
    if (!Number.isFinite(parsed)) error = `${field.label} must be a number.`;
    else value = parsed;
  } else if (field.type === "boolean") {
    if (typeof input !== "boolean") error = `${field.label} must be a boolean.`;
  } else if (typeof input !== "string") {
    error = `${field.label} must be text.`;
  } else if (field.required === true && input.trim() === "") {
    error = `${field.label} is required.`;
  }
  const errors = { ...draft.errors };
  if (error === undefined) delete errors[key];
  else errors[key] = error;
  return { ...draft, values: { ...draft.values, [key]: value }, errors };
}

export function commitPropertyDraft(
  state: WorkflowEditorState,
  draft: WorkflowPropertyDraft,
): WorkflowEditorState {
  if (Object.keys(draft.errors).length > 0)
    throw new Error("workflow property draft contains validation errors");
  return editWorkflow(state, (document) => ({
    ...document,
    nodes: document.nodes.map((node, index) =>
      workflowNodeId(node, index) === draft.nodeId
        ? { ...node, ...draft.values }
        : node,
    ),
  }));
}

export function validateWorkflow(
  document: WorkflowDocument,
): readonly WorkflowValidationIssue[] {
  const issues: WorkflowValidationIssue[] = [];
  if (document.schemaVersion !== 1)
    issues.push({
      code: "unsupported_schema",
      itemId: "workflow",
      message: `Workflow schemaVersion ${document.schemaVersion} is inspectable and exportable, but this build edits only v1.`,
    });
  const nodeIds = new Set<string>();
  document.nodes.forEach((node, index) => {
    const id = nodeId(node, index);
    if (
      typeof node.id !== "string" ||
      node.id.trim() === "" ||
      typeof node.type !== "string" ||
      node.type.trim() === ""
    )
      issues.push({
        code: "invalid_node",
        itemId: id,
        message: "Node requires stable id and type strings.",
      });
    if (nodeIds.has(id))
      issues.push({
        code: "duplicate_node",
        itemId: id,
        message: `Duplicate node id ${id}.`,
      });
    nodeIds.add(id);
    if (node.capabilityStatus === "missing")
      issues.push({
        code: "missing_dependency",
        itemId: id,
        message: `${String(node.requirement ?? node.label ?? id)} is not configured.`,
      });
    if (
      node.configuration !== undefined &&
      (typeof node.configuration !== "object" ||
        node.configuration === null ||
        Array.isArray(node.configuration))
    )
      issues.push({
        code: "invalid_configuration",
        itemId: id,
        message: `${id} configuration must be a JSON object.`,
      });
  });
  const edgeIds = new Set<string>();
  document.edges.forEach((edge, index) => {
    const id = edgeId(edge, index);
    if (edge.id !== undefined) {
      if (typeof edge.id !== "string" || edge.id.trim() === "")
        issues.push({
          code: "invalid_edge",
          itemId: id,
          message: "Transition id must be a non-empty string when present.",
        });
      else if (edgeIds.has(edge.id))
        issues.push({
          code: "duplicate_edge",
          itemId: id,
          message: `Duplicate transition id ${id}.`,
        });
      else edgeIds.add(edge.id);
    }
    if (
      typeof edge.source !== "string" ||
      edge.source.trim() === "" ||
      typeof edge.target !== "string" ||
      edge.target.trim() === ""
    ) {
      issues.push({
        code: "invalid_edge",
        itemId: id,
        message: "Transition requires source and target node IDs.",
      });
      return;
    }
    if (!nodeIds.has(edge.source))
      issues.push({
        code: "missing_source",
        itemId: id,
        message: `Transition source ${edge.source} does not exist.`,
      });
    if (!nodeIds.has(edge.target))
      issues.push({
        code: "missing_target",
        itemId: id,
        message: `Transition target ${edge.target} does not exist.`,
      });
    if (!nodeIds.has(edge.source) || !nodeIds.has(edge.target)) return;
    const sourceNode = document.nodes.find(
      (node, nodeIndex) => nodeId(node, nodeIndex) === edge.source,
    );
    const targetNode = document.nodes.find(
      (node, nodeIndex) => nodeId(node, nodeIndex) === edge.target,
    );
    const sourceType =
      typeof sourceNode?.type === "string" ? sourceNode.type : undefined;
    const targetType =
      typeof targetNode?.type === "string" ? targetNode.type : undefined;
    const sourcePort = typeof edge.sourcePort === "string" ? edge.sourcePort : "out";
    const targetPort = typeof edge.targetPort === "string" ? edge.targetPort : "in";
    const route = typeof edge.route === "string" ? edge.route : undefined;
    if (
      sourceType === "condition" &&
      route !== "true" &&
      route !== "false" &&
      sourcePort !== "true" &&
      sourcePort !== "false"
    )
      issues.push({
        code: "condition_route_missing",
        itemId: id,
        message:
          "A condition transition must route true or false through a typed handle or route label.",
      });
    const sourceKind = resolveOutputPortKind(sourceType, sourcePort);
    const targetKind = resolveInputPortKind(targetType, targetPort);
    if (sourceKind === "unknown-port")
      issues.push({
        code: "unknown_port",
        itemId: id,
        message: `Transition source port ${sourcePort} does not exist on a ${sourceType ?? "node"}.`,
      });
    if (targetKind === "unknown-port")
      issues.push({
        code: "unknown_port",
        itemId: id,
        message: `Transition target port ${targetPort} does not exist on a ${targetType ?? "node"}.`,
      });
    if (
      sourceKind !== undefined &&
      sourceKind !== "unknown-port" &&
      targetKind !== undefined &&
      targetKind !== "unknown-port" &&
      !portKindsConnect(sourceKind, targetKind)
    )
      issues.push({
        code: "connection_type_mismatch",
        itemId: id,
        message: `Transition connects an incompatible ${sourceKind} output to a ${targetKind} input.`,
      });
  });
  const cycleNodes = nodesInCycles(document);
  for (const nodeIdValue of cycleNodes) {
    issues.push({
      code: "cycle_detected",
      itemId: nodeIdValue,
      message: `Node ${nodeIdValue} participates in a cycle; a workflow pass must be acyclic.`,
    });
  }
  return issues;
}

/** Returns node IDs that participate in at least one directed edge cycle. */
export function nodesInCycles(document: WorkflowDocument): readonly string[] {
  const ids = document.nodes.map((node, index) => nodeId(node, index));
  const idSet = new Set(ids);
  const adjacency = new Map<string, string[]>(
    ids.map((id) => [id, [] as string[]]),
  );
  const indegree = new Map<string, number>(ids.map((id) => [id, 0]));
  document.edges.forEach((edge) => {
    const source = edgeIdSource(edge);
    const target = edgeIdTarget(edge);
    if (source !== null && target !== null && idSet.has(source) && idSet.has(target)) {
      adjacency.get(source)?.push(target);
      indegree.set(target, (indegree.get(target) ?? 0) + 1);
    }
  });
  const queue = ids.filter((id) => (indegree.get(id) ?? 0) === 0);
  const ordered = new Set<string>();
  for (let head = 0; head < queue.length; head += 1) {
    const node = queue[head]!;
    ordered.add(node);
    for (const next of adjacency.get(node) ?? []) {
      const remaining = (indegree.get(next) ?? 0) - 1;
      indegree.set(next, remaining);
      if (remaining === 0) queue.push(next);
    }
  }
  if (ordered.size === ids.length) return [];
  return ids.filter((id) => !ordered.has(id)).sort();
}

function edgeIdSource(edge: JsonObject): string | null {
  return typeof edge.source === "string" ? edge.source : null;
}
function edgeIdTarget(edge: JsonObject): string | null {
  return typeof edge.target === "string" ? edge.target : null;
}
export function workflowSummary(document: WorkflowDocument): WorkflowSummary {
  return {
    nodes: document.nodes.length,
    edges: document.edges.length,
    unresolved: document.nodes.filter(
      (node) => node.capabilityStatus === "missing",
    ).length,
    issues: validateWorkflow(document).length,
  };
}
export function selectedWorkflowNode(
  state: WorkflowEditorState,
): JsonObject | null {
  const selected = state.selectedIds.values().next().value as
    | string
    | undefined;
  return selected === undefined
    ? null
    : (state.document.nodes.find(
        (node, index) => nodeId(node, index) === selected,
      ) ?? null);
}
export function selectedWorkflowEdge(
  state: WorkflowEditorState,
): JsonObject | null {
  const selected = state.selectedIds.values().next().value as
    | string
    | undefined;
  return selected === undefined
    ? null
    : (state.document.edges.find(
        (edge, index) => edgeId(edge, index) === selected,
      ) ?? null);
}
export function workflowNodeId(node: JsonObject, fallback: number): string {
  return nodeId(node, fallback);
}
export function workflowEdgeId(edge: JsonObject, fallback: number): string {
  return edgeId(edge, fallback);
}

function nodeId(node: JsonObject, fallback: number): string {
  return typeof node.id === "string" ? node.id : `node-${fallback}`;
}
function edgeId(edge: JsonObject, fallback: number): string {
  return typeof edge.id === "string" ? edge.id : `edge-${fallback}`;
}
function uniqueEdgeId(
  document: WorkflowDocument,
  source: string,
  target: string,
): string {
  const existing = new Set(document.edges.map(edgeId));
  let ordinal = 1;
  let candidate = `${source}-${target}-${ordinal}`;
  while (existing.has(candidate))
    candidate = `${source}-${target}-${++ordinal}`;
  return candidate;
}
function labelFor(type: string): string {
  return type
    .replaceAll("_", " ")
    .replace(/^./, (value) => value.toUpperCase());
}

/** Seeds a typed node's configuration from its catalog default when known. */
function defaultConfigurationFor(type: string): JsonObject {
  const entry = catalogEntryForType(type);
  return entry === undefined ? {} : { ...entry.defaultConfiguration };
}
