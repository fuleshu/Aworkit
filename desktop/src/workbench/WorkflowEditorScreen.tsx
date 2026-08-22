import { useEffect, useMemo, useRef, useState } from "react";
import {
  createWorkflowCorePort,
  nextWorkbenchCommandId,
  type WorkflowCorePort,
} from "./corePort";
import { WorkflowGraphSurfaceAdapter } from "./graphSurface";
import {
  addWorkflowNode,
  clearWorkflowSelection,
  connectWorkflowNodes,
  createEditor,
  deleteSelectedWorkflowItems,
  moveWorkflowNode,
  parseWorkflow,
  redoWorkflow,
  selectedWorkflowNode,
  selectWorkflowNode,
  undoWorkflow,
  updateSelectedNodeProperty,
  updateSelectedNodeFields,
  validateWorkflow,
  workflowNodeId,
  workflowSummary,
  type WorkflowDocument,
} from "./workflow";

interface WorkflowEditorScreenProps {
  readonly document: WorkflowDocument;
  readonly workflowPort?: WorkflowCorePort;
  readonly onOpenSettings?: () => void;
  readonly onRun?: () => void;
}

/** Full lossless workflow editor composed over the Aworkit kernel and replaceable graph port. */
export function WorkflowEditorScreen({
  document,
  workflowPort,
  onOpenSettings,
  onRun,
}: WorkflowEditorScreenProps): React.JSX.Element {
  const port = useMemo(
    () => workflowPort ?? createWorkflowCorePort(document),
    [document, workflowPort],
  );
  const [editor, setEditor] = useState(() => createSelectedEditor(document));
  const [savedRevision, setSavedRevision] = useState(0);
  const [projectedVersion, setProjectedVersion] = useState(0);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [retryCommandId, setRetryCommandId] = useState<string | null>(null);
  const [paletteFilter, setPaletteFilter] = useState("");
  const [edgeSource, setEdgeSource] = useState("");
  const [edgeTarget, setEdgeTarget] = useState("");
  const importInput = useRef<HTMLInputElement>(null);
  const graph = useMemo(() => new WorkflowGraphSurfaceAdapter(), []);
  useEffect(() => {
    let active = true;
    void port
      .snapshot()
      .then((snapshot) => {
        if (!active) return;
        setProjectedVersion(snapshot.version);
        setEditor(createSelectedEditor(snapshot.document));
        setSavedRevision(0);
        setError(null);
      })
      .catch((failure: unknown) => {
        if (active)
          setError(
            failure instanceof Error ? failure.message : String(failure),
          );
      });
    return () => {
      active = false;
    };
  }, [port]);
  const selected = selectedWorkflowNode(editor);
  const summary = workflowSummary(editor.document);
  const issues = validateWorkflow(editor.document);
  const blockingIssues = issues.filter(
    (issue) => issue.code !== "missing_dependency",
  );
  const missingIssue = issues.find(
    (issue) => issue.code === "missing_dependency",
  );
  const save = async () => {
    if (blockingIssues.length > 0 || savedRevision === editor.revision) return;
    const commandId = retryCommandId ?? nextWorkbenchCommandId("workflow");
    setRetryCommandId(commandId);
    setSaving(true);
    try {
      const receipt = await port.commit({
        commandId,
        expectedVersion: projectedVersion,
        document: editor.document,
      });
      if (!receipt.accepted) {
        setRetryCommandId(null);
        setError(receipt.reason ?? "The trusted core rejected the workflow.");
        return;
      }
      setProjectedVersion(receipt.currentVersion);
      setSavedRevision(editor.revision);
      setRetryCommandId(null);
      setError(null);
    } catch (failure) {
      const failureMessage =
        failure instanceof Error ? failure.message : String(failure);
      setError(failureMessage);
      if (failureMessage.includes("version conflict")) {
        setRetryCommandId(null);
        try {
          setProjectedVersion((await port.snapshot()).version);
        } catch {
          // Preserve the complete local document until a fresh version arrives.
        }
      }
    } finally {
      setSaving(false);
    }
  };
  const exportDocument = () => {
    const url = URL.createObjectURL(
      new Blob([JSON.stringify(editor.document, null, 2)], {
        type: "application/json",
      }),
    );
    const anchor = window.document.createElement("a");
    anchor.href = url;
    anchor.download = "repository-engineer.aworkit.json";
    anchor.click();
    URL.revokeObjectURL(url);
  };
  return (
    <section className="workflow-editor">
      <header className="surface-toolbar">
        <div>
          <p className="eyebrow">PROJECT ATLAS · WORKFLOW</p>
          <h1>Repository Engineer</h1>
        </div>
        <div className="toolbar-actions">
          <span>Version {projectedVersion}</span>
          <button
            title="Import a lossless Aworkit workflow JSON document"
            type="button"
            onClick={() => importInput.current?.click()}
          >
            Import…
          </button>
          <button
            disabled={editor.undo.length === 0}
            title="Undo the last workflow transaction"
            type="button"
            onClick={() => setEditor(undoWorkflow)}
          >
            ↶ Undo
          </button>
          <button
            disabled={editor.redo.length === 0}
            title="Redo the last undone transaction"
            type="button"
            onClick={() => setEditor(redoWorkflow)}
          >
            ↷ Redo
          </button>
          <button
            title="Validate the complete lossless workflow document"
            type="button"
            onClick={() => {
              const first = issues[0];
              if (first !== undefined)
                setEditor((state) => selectWorkflowNode(state, first.itemId));
            }}
          >
            Validate{" "}
            <span
              className={issues.length > 0 ? "count-warning" : "count-success"}
            >
              {issues.length}
            </span>
          </button>
          <button
            className="primary-action"
            disabled={
              saving ||
              blockingIssues.length > 0 ||
              savedRevision === editor.revision
            }
            title={
              blockingIssues.length > 0
                ? "Resolve structural validation errors before saving"
                : "Save with an optimistic document version"
            }
            type="button"
            onClick={() => void save()}
          >
            Save
          </button>
          <button
            title="Export the complete lossless workflow JSON document"
            type="button"
            onClick={exportDocument}
          >
            Export
          </button>
          <button
            disabled={
              issues.length > 0 ||
              savedRevision !== editor.revision ||
              onRun === undefined
            }
            title={
              issues.length > 0
                ? "Resolve every dependency and validation issue before running"
                : savedRevision !== editor.revision
                  ? "Save the resolved workflow before starting a Run"
                  : "Open a new Chat with this saved workflow selected"
            }
            type="button"
            onClick={onRun}
          >
            Run
          </button>
        </div>
      </header>
      <input
        accept="application/json,.json"
        hidden
        ref={importInput}
        type="file"
        onChange={(event) => {
          const file = event.target.files?.[0];
          if (file === undefined) return;
          void file
            .text()
            .then((raw) => {
              setEditor(createSelectedEditor(parseWorkflow(raw)));
              setSavedRevision(-1);
              setRetryCommandId(null);
              setError(null);
            })
            .catch((failure: unknown) =>
              setError(
                failure instanceof Error ? failure.message : String(failure),
              ),
            );
          event.target.value = "";
        }}
      />
      {error !== null && (
        <div className="command-banner" role="status">
          {error} The lossless local document remains available. Uncertain
          delivery retries use the same command ID; a known version conflict is
          rebased as a new command.
        </div>
      )}
      <div className="workflow-body">
        {missingIssue !== undefined && (
          <button
            className="dependency-banner"
            title="Select the unresolved workflow dependency"
            type="button"
            onClick={() =>
              setEditor((state) =>
                selectWorkflowNode(state, missingIssue.itemId),
              )
            }
          >
            <span>!</span>
            <strong>Missing dependency</strong>
            {missingIssue.message}
          </button>
        )}
        <aside className="node-palette">
          <header>
            <strong>Nodes</strong>
            <input
              aria-label="Filter workflow nodes"
              placeholder="Filter nodes"
              title="Filter the workflow node palette"
              value={paletteFilter}
              onChange={(event) => setPaletteFilter(event.target.value)}
            />
          </header>
          {[
            {
              group: "FLOW",
              items: [
                ["input", "Input"],
                ["gate", "Approval gate"],
                ["join", "Join"],
              ],
            },
            {
              group: "AGENTS",
              items: [
                ["model", "Model"],
                ["agent", "Agent"],
                ["subagent", "Subagent"],
              ],
            },
            {
              group: "TOOLS",
              items: [
                ["tool", "Tool"],
                ["mcp", "MCP"],
                ["shell", "Shell"],
              ],
            },
            {
              group: "EXTENSIONS",
              items: [
                ["plugin", "Plugin"],
                ["output", "Output"],
              ],
            },
          ]
            .map((section) => ({
              ...section,
              items: section.items.filter(([, label]) =>
                label.toLowerCase().includes(paletteFilter.toLowerCase()),
              ),
            }))
            .filter((section) => section.items.length > 0)
            .map((section) => (
              <section key={section.group}>
                <small>{section.group}</small>
                {section.items.map(([type, label]) => (
                  <button
                    aria-label={`Add a ${label} node to the canvas`}
                    draggable
                    key={type}
                    title={`Add a ${label} node to the canvas`}
                    type="button"
                    onDragStart={(event) => {
                      event.dataTransfer.effectAllowed = "copy";
                      event.dataTransfer.setData(
                        "application/x-aworkit-node",
                        type,
                      );
                    }}
                    onClick={() =>
                      setEditor((state) =>
                        addWorkflowNode(state, type, {
                          x: 180 + state.document.nodes.length * 24,
                          y: 120 + state.document.nodes.length * 16,
                        }),
                      )
                    }
                  >
                    <span>{type === "model" ? "AI" : "◆"}</span>
                    {label}
                  </button>
                ))}
              </section>
            ))}
          <details className="workflow-outline" open>
            <summary>Canvas outline</summary>
            <ul aria-label="Workflow nodes">
              {editor.document.nodes.map((node, index) => {
                const id = workflowNodeId(node, index);
                return (
                  <li key={id}>
                    <button
                      aria-pressed={editor.selectedIds.has(id)}
                      title="Select this node; Alt+Arrow moves it by eight canvas units"
                      type="button"
                      onClick={() =>
                        setEditor((state) => selectWorkflowNode(state, id))
                      }
                      onKeyDown={(event) => {
                        if (!event.altKey || !event.key.startsWith("Arrow"))
                          return;
                        event.preventDefault();
                        setEditor((state) => {
                          const current = state.document.nodes.find(
                            (candidate, candidateIndex) =>
                              workflowNodeId(candidate, candidateIndex) === id,
                          );
                          const point = positionOf(current?.position);
                          return moveWorkflowNode(state, id, {
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
                From
                <select
                  aria-label="Transition source node"
                  value={edgeSource}
                  onChange={(event) => setEdgeSource(event.target.value)}
                >
                  <option value="">Select…</option>
                  {editor.document.nodes.map((node, index) => {
                    const id = workflowNodeId(node, index);
                    return (
                      <option key={id} value={id}>
                        {String(node.label ?? id)}
                      </option>
                    );
                  })}
                </select>
              </label>
              <label>
                To
                <select
                  aria-label="Transition target node"
                  value={edgeTarget}
                  onChange={(event) => setEdgeTarget(event.target.value)}
                >
                  <option value="">Select…</option>
                  {editor.document.nodes.map((node, index) => {
                    const id = workflowNodeId(node, index);
                    return (
                      <option key={id} value={id}>
                        {String(node.label ?? id)}
                      </option>
                    );
                  })}
                </select>
              </label>
              <button
                disabled={edgeSource === "" || edgeTarget === ""}
                title="Add a default typed transition; self-loops and multi-edges are preserved"
                type="button"
                onClick={() => {
                  setEditor((state) =>
                    connectWorkflowNodes(state, edgeSource, edgeTarget),
                  );
                  setEdgeSource("");
                  setEdgeTarget("");
                }}
              >
                Add transition
              </button>
            </div>
            <ol aria-label="Workflow transitions">
              {editor.document.edges.map((edge, index) => (
                <li key={String(edge.id ?? `edge-${index}`)}>
                  {String(edge.source ?? "?")} → {String(edge.target ?? "?")}
                </li>
              ))}
            </ol>
          </details>
        </aside>
        {graph.render(editor, {
          onAdd: (type, position) =>
            setEditor((state) => addWorkflowNode(state, type, position)),
          onSelect: (id) => setEditor((state) => selectWorkflowNode(state, id)),
          onClearSelection: () => setEditor(clearWorkflowSelection),
          onMove: (id, position) =>
            setEditor((state) => moveWorkflowNode(state, id, position)),
          onConnect: (source, target, sourceHandle, targetHandle) =>
            setEditor((state) =>
              connectWorkflowNodes(
                state,
                source,
                target,
                sourceHandle,
                targetHandle,
              ),
            ),
        })}
        <aside className="properties-pane">
          <header>
            <div>
              <p className="eyebrow">PROPERTIES</p>
              <h2>
                {selected === null
                  ? "Workflow"
                  : String(selected.label ?? selected.id)}
              </h2>
            </div>
            {selected !== null && (
              <button
                title="Delete the selected node and connected transitions"
                type="button"
                onClick={() => setEditor(deleteSelectedWorkflowItems)}
              >
                Delete
              </button>
            )}
          </header>
          {selected === null ? (
            <div className="property-content">
              <dl>
                <div>
                  <dt>Nodes</dt>
                  <dd>{summary.nodes}</dd>
                </div>
                <div>
                  <dt>Transitions</dt>
                  <dd>{summary.edges}</dd>
                </div>
                <div>
                  <dt>Dependencies</dt>
                  <dd>{summary.unresolved} unresolved</dd>
                </div>
              </dl>
              <h3>Validation</h3>
              {issues.length === 0 ? (
                <p className="success-copy">✓ Document is valid</p>
              ) : (
                <ul>
                  {issues.map((issue) => (
                    <li key={`${issue.code}-${issue.itemId}`}>
                      {issue.message}
                    </li>
                  ))}
                </ul>
              )}
            </div>
          ) : (
            <div className="property-content">
              <label>
                Label
                <input
                  title="Edit the selected node label"
                  value={String(selected.label ?? "")}
                  onChange={(event) =>
                    setEditor((state) =>
                      updateSelectedNodeProperty(
                        state,
                        "label",
                        event.target.value,
                      ),
                    )
                  }
                />
              </label>
              <label>
                Type
                <input
                  title="Edit the selected node type"
                  value={String(selected.type ?? "")}
                  onChange={(event) =>
                    setEditor((state) =>
                      updateSelectedNodeProperty(
                        state,
                        "type",
                        event.target.value,
                      ),
                    )
                  }
                />
              </label>
              {selected.capabilityStatus === "missing" && (
                <section className="dependency-resolution">
                  <span className="status incompatible">Missing</span>
                  <label>
                    Logical requirement
                    <input
                      readOnly
                      title="The unresolved logical capability requirement"
                      value={String(
                        selected.requirement ?? selected.label ?? selected.id,
                      )}
                    />
                  </label>
                  <button
                    disabled={onOpenSettings === undefined}
                    title="Open Settings to configure a compatible capability"
                    type="button"
                    onClick={onOpenSettings}
                  >
                    Configure compatible capability…
                  </button>
                  <button
                    title="Replace this unresolved dependency with the built-in project-files tool"
                    type="button"
                    onClick={() =>
                      setEditor((state) =>
                        updateSelectedNodeFields(state, {
                          type: "tool",
                          label: "Project files",
                          requirement: "tool.files",
                          capabilityStatus: "ready",
                        }),
                      )
                    }
                  >
                    Replace with Project files
                  </button>
                </section>
              )}
              <h3>Authority summary</h3>
              <p>
                No authority is inherited from this document. At Run start the
                trusted core freezes only explicitly resolved, workspace-scoped
                capabilities.
              </p>
              <h3>Raw fields</h3>
              <pre>{JSON.stringify(selected, null, 2)}</pre>
            </div>
          )}
        </aside>
      </div>
    </section>
  );
}

function createSelectedEditor(document: WorkflowDocument) {
  const editor = createEditor(document);
  const missing = document.nodes.findIndex(
    (node) => node.capabilityStatus === "missing",
  );
  if (missing < 0) return editor;
  const id = document.nodes[missing]?.id;
  return selectWorkflowNode(
    editor,
    typeof id === "string" ? id : `node-${missing}`,
  );
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
