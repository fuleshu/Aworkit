/** Lossless workflow-document editor kernel. Unknown JSON always round-trips. */
export type JsonValue = null | boolean | number | string | readonly JsonValue[] | { readonly [key: string]: JsonValue };
export type JsonObject = { [key: string]: JsonValue };
export interface WorkflowDocument { readonly schemaVersion: number; readonly nodes: readonly JsonObject[]; readonly edges: readonly JsonObject[]; readonly [key: string]: JsonValue; }
export interface WorkflowEditorState { readonly document: WorkflowDocument; readonly selectedIds: ReadonlySet<string>; readonly undo: readonly WorkflowDocument[]; readonly redo: readonly WorkflowDocument[]; }

function clone<T>(value: T): T { return JSON.parse(JSON.stringify(value)) as T; }
function asDocument(value: unknown): WorkflowDocument { if (typeof value !== "object" || value === null || Array.isArray(value)) throw new Error("workflow must be a JSON object"); const document = clone(value as WorkflowDocument); if (!Number.isInteger(document.schemaVersion) || !Array.isArray(document.nodes) || !Array.isArray(document.edges)) throw new Error("workflow requires schemaVersion, nodes, and edges"); return document; }
export function parseWorkflow(raw: string): WorkflowDocument { return asDocument(JSON.parse(raw)); }
export function serializeWorkflow(document: WorkflowDocument): string { return JSON.stringify(document); }
export function createEditor(document: WorkflowDocument): WorkflowEditorState { return { document: asDocument(document), selectedIds: new Set(), undo: [], redo: [] }; }

/** Applies one immutable JSON-preserving mutation and coalesces it into undo history. */
export function editWorkflow(state: WorkflowEditorState, mutate: (document: WorkflowDocument) => WorkflowDocument): WorkflowEditorState { const document = asDocument(mutate(clone(state.document))); return { ...state, document, undo: [...state.undo, state.document], redo: [] }; }
export function undoWorkflow(state: WorkflowEditorState): WorkflowEditorState { const previous = state.undo.at(-1); return previous === undefined ? state : { ...state, document: previous, undo: state.undo.slice(0, -1), redo: [state.document, ...state.redo] }; }
export function redoWorkflow(state: WorkflowEditorState): WorkflowEditorState { const next = state.redo[0]; return next === undefined ? state : { ...state, document: next, undo: [...state.undo, state.document], redo: state.redo.slice(1) }; }
export function selectWorkflowNode(state: WorkflowEditorState, nodeId: string): WorkflowEditorState { return { ...state, selectedIds: new Set([nodeId]) }; }
export function workflowSummary(document: WorkflowDocument): { readonly nodes: number; readonly edges: number; readonly unresolved: number } { const unresolved = document.nodes.filter((node) => node.capabilityStatus === "missing").length; return { nodes: document.nodes.length, edges: document.edges.length, unresolved }; }
