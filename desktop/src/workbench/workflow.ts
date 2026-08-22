/** Framework-independent, lossless workflow-document editor kernel. */
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
    | "duplicate_node"
    | "missing_source"
    | "missing_target"
    | "invalid_node"
    | "invalid_edge"
    | "missing_dependency";
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
function asDocument(value: unknown): WorkflowDocument {
  if (typeof value !== "object" || value === null || Array.isArray(value))
    throw new Error("workflow must be a JSON object");
  const document = clone(value as WorkflowDocument);
  if (
    !Number.isInteger(document.schemaVersion) ||
    document.schemaVersion < 1 ||
    !Array.isArray(document.nodes) ||
    !Array.isArray(document.edges)
  )
    throw new Error(
      "workflow requires a positive schemaVersion plus nodes and edges arrays",
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
  let counter = state.document.nodes.length + 1;
  let id = `${type}.${counter}`;
  while (existing.has(id)) id = `${type}.${++counter}`;
  return editWorkflow(state, (document) => ({
    ...document,
    nodes: [
      ...document.nodes,
      {
        id,
        type,
        label: labelFor(type),
        position: { x: position.x, y: position.y },
        config: {},
      },
    ],
  }));
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
  return editWorkflow(state, (document) => ({
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
}
export function deleteSelectedWorkflowItems(
  state: WorkflowEditorState,
): WorkflowEditorState {
  const ids = state.selectedIds;
  return {
    ...editWorkflow(state, (document) => ({
      ...document,
      nodes: document.nodes.filter(
        (node, index) => !ids.has(nodeId(node, index)),
      ),
      edges: document.edges.filter((edge, index) => {
        const id = edgeId(edge, index);
        return (
          !ids.has(id) &&
          !(typeof edge.source === "string" && ids.has(edge.source)) &&
          !(typeof edge.target === "string" && ids.has(edge.target))
        );
      }),
    })),
    selectedIds: new Set(),
  };
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
  const nodeIds = new Set<string>();
  document.nodes.forEach((node, index) => {
    const id = nodeId(node, index);
    if (typeof node.id !== "string" || typeof node.type !== "string")
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
  });
  document.edges.forEach((edge, index) => {
    const id = edgeId(edge, index);
    if (typeof edge.source !== "string" || typeof edge.target !== "string") {
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
  });
  return issues;
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
