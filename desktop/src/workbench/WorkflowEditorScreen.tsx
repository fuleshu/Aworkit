import { useEffect, useMemo, useState } from "react";
import {
  createWorkflowCorePort,
  nextWorkbenchCommandId,
  type WorkflowCorePort,
} from "./corePort";
import { WorkflowGraphSurfaceAdapter } from "./graphSurface";
import { WorkflowPalette } from "./WorkflowPalette";
import { WorkflowPropertiesPane } from "./WorkflowPropertiesPane";
import { WorkflowToolbar } from "./WorkflowToolbar";
import {
  addWorkflowNode,
  clearWorkflowSelection,
  connectWorkflowNodes,
  createEditor,
  deleteSelectedWorkflowItems,
  deleteWorkflowItems,
  editWorkflow,
  moveWorkflowNode,
  parseWorkflow,
  redoWorkflow,
  renameWorkflowNode,
  replaceWorkflowDocument,
  selectedWorkflowEdge,
  selectedWorkflowNode,
  selectWorkflowItem,
  serializeWorkflow,
  undoWorkflow,
  updateSelectedEdgeFields,
  updateSelectedNodeField,
  updateSelectedNodeProperty,
  validateWorkflow,
  workflowSummary,
  type WorkflowDocument,
} from "./workflow";
import { assessNativeWorkflow } from "./workflowExecution";

interface WorkflowEditorScreenProps {
  readonly document: WorkflowDocument;
  readonly workflowPort?: WorkflowCorePort;
  readonly onOpenSettings?: () => void;
  readonly onRun?: () => void;
  readonly runBlockedReason?: string;
}

/** Lossless visual document editor with an explicit native-execution gate. */
export function WorkflowEditorScreen({
  document,
  workflowPort,
  onOpenSettings,
  onRun,
  runBlockedReason,
}: WorkflowEditorScreenProps): React.JSX.Element {
  const port = useMemo(
    () => workflowPort ?? createWorkflowCorePort(document),
    [document, workflowPort],
  );
  const [editor, setEditor] = useState(() => createSelectedEditor(document));
  const [savedFingerprint, setSavedFingerprint] = useState(() =>
    serializeWorkflow(document),
  );
  const [projectedVersion, setProjectedVersion] = useState(0);
  const [storedEditable, setStoredEditable] = useState(
    document.schemaVersion === 1,
  );
  const [saving, setSaving] = useState(false);
  const [pendingPropertyDraft, setPendingPropertyDraft] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [retryCommandId, setRetryCommandId] = useState<string | null>(null);
  const graph = useMemo(() => new WorkflowGraphSurfaceAdapter(), []);

  useEffect(() => {
    let active = true;
    void port
      .snapshot()
      .then((snapshot) => {
        if (!active) return;
        setProjectedVersion(snapshot.version);
        setStoredEditable(snapshot.editable);
        setEditor(createSelectedEditor(snapshot.document));
        setSavedFingerprint(serializeWorkflow(snapshot.document));
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

  const selectedId =
    (editor.selectedIds.values().next().value as string | undefined) ?? null;
  const selectedNode = selectedWorkflowNode(editor);
  const selectedEdge = selectedWorkflowEdge(editor);
  const summary = workflowSummary(editor.document);
  const issues = validateWorkflow(editor.document);
  const saveBlockingIssues = issues.filter(
    (issue) => issue.code !== "missing_dependency",
  );
  const missingIssue = issues.find(
    (issue) => issue.code === "missing_dependency",
  );
  const compatibility = assessNativeWorkflow(editor.document);
  const documentEditable =
    storedEditable && editor.document.schemaVersion === 1;
  const fingerprint = serializeWorkflow(editor.document);
  const dirty = fingerprint !== savedFingerprint;
  const validationCount =
    issues.length + (compatibility.executable ? 0 : compatibility.issues.length);
  const workflowName =
    typeof editor.document.name === "string"
      ? editor.document.name
      : "Untitled workflow";

  const save = async () => {
    if (
      saveBlockingIssues.length > 0 ||
      !documentEditable ||
      pendingPropertyDraft ||
      !dirty ||
      saving
    )
      return;
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
      setStoredEditable(true);
      setSavedFingerprint(fingerprint);
      setRetryCommandId(null);
      setError(null);
      setNotice("Workflow saved by the trusted core.");
    } catch (failure) {
      const failureMessage =
        failure instanceof Error ? failure.message : String(failure);
      setError(failureMessage);
      if (failureMessage.includes("version conflict")) {
        setRetryCommandId(null);
        try {
          setProjectedVersion((await port.snapshot()).version);
        } catch {
          // The complete local document stays available for export or retry.
        }
      }
    } finally {
      setSaving(false);
    }
  };

  const importDocument = async (file: File) => {
    try {
      const imported = parseWorkflow(await readFile(file));
      setEditor((state) => replaceWorkflowDocument(state, imported));
      setError(null);
      setNotice(
        imported.schemaVersion === 1
          ? `Imported ${file.name} locally. Import never installs or enables node implementations; validate, then Save or Export.`
          : `Imported ${file.name} as a read-only future-schema document. It remains inspectable and losslessly exportable, but this build will not edit or save it.`,
      );
    } catch (failure) {
      setError(
        `Import failed: ${failure instanceof Error ? failure.message : String(failure)}`,
      );
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
    anchor.download = `${fileSafeName(workflowName)}.aworkit.json`;
    window.document.body.append(anchor);
    anchor.click();
    anchor.remove();
    URL.revokeObjectURL(url);
    setNotice("Exported the complete workflow JSON, including unknown fields.");
  };

  const selectValidationResult = () => {
    const first = issues[0];
    if (first !== undefined) {
      setEditor((state) => selectWorkflowItem(state, first.itemId));
      setNotice(first.message);
    } else if (!compatibility.executable) {
      setEditor(clearWorkflowSelection);
      setNotice(
        `Document is valid and savable, but not executable in this runtime. ${compatibility.issues[0]?.message ?? ""}`,
      );
    } else {
      setNotice("Validation passed: this document is executable as Simple Chat.");
    }
  };

  const saveTitle = saveTitleFor(
    saveBlockingIssues.length,
    documentEditable,
    pendingPropertyDraft,
    dirty,
  );
  const canRun =
    documentEditable &&
    compatibility.executable &&
    issues.length === 0 &&
    !pendingPropertyDraft &&
    !dirty &&
    runBlockedReason === undefined &&
    onRun !== undefined;

  return (
    <section className="workflow-editor">
      <WorkflowToolbar
        canRedo={editor.redo.length > 0}
        canUndo={editor.undo.length > 0}
        editable={documentEditable}
        executable={compatibility.executable}
        projectedVersion={projectedVersion}
        runDisabled={!canRun}
        runTitle={
          runBlockedReason ??
          runTitleFor(
            documentEditable,
            compatibility.executable,
            issues.length,
            pendingPropertyDraft,
            dirty,
            onRun !== undefined,
          )
        }
        saveDisabled={
          saving ||
          saveBlockingIssues.length > 0 ||
          !documentEditable ||
          pendingPropertyDraft ||
          !dirty
        }
        saveTitle={saveTitle}
        saving={saving}
        validationCount={validationCount}
        workflowName={workflowName}
        onExport={exportDocument}
        onImport={(file) => void importDocument(file)}
        onRedo={() => setEditor(redoWorkflow)}
        onRun={onRun}
        onSave={() => void save()}
        onUndo={() => setEditor(undoWorkflow)}
        onValidate={selectValidationResult}
      />
      {error !== null && (
        <div className="command-banner error-banner" role="alert">
          {error} The complete local document remains available for Undo or
          Export.
        </div>
      )}
      {error === null && notice !== null && (
        <div className="command-banner" role="status">
          {notice}
        </div>
      )}
      <div className="workflow-body">
        {!documentEditable ? (
          <div className="workflow-runtime-banner" role="status">
            <strong>Read-only workflow document.</strong>
            <span>
              This stored or future schema is preserved for inspection and
              lossless export; this build will not overwrite it.
            </span>
          </div>
        ) : !compatibility.executable ? (
          <div className="workflow-runtime-banner" role="status">
            <strong>Editable document; native execution is limited.</strong>
            <span>{compatibility.issues[0]?.message}</span>
          </div>
        ) : null}
        {missingIssue !== undefined && (
          <button
            className="dependency-banner"
            title="Select the unresolved workflow dependency"
            type="button"
            onClick={() =>
              setEditor((state) =>
                selectWorkflowItem(state, missingIssue.itemId),
              )
            }
          >
            <span>!</span>
            <strong>Missing dependency</strong>
            {missingIssue.message}
          </button>
        )}
        <WorkflowPalette
          document={editor.document}
          editable={documentEditable}
          selectedIds={editor.selectedIds}
          onAddNode={(type, position) =>
            setEditor((state) => addWorkflowNode(state, type, position))
          }
          onConnect={(source, target) =>
            setEditor((state) => connectWorkflowNodes(state, source, target))
          }
          onDelete={(ids) =>
            setEditor((state) => deleteWorkflowItems(state, new Set(ids)))
          }
          onMove={(id, position) =>
            setEditor((state) => moveWorkflowNode(state, id, position))
          }
          onSelect={(id) =>
            setEditor((state) => selectWorkflowItem(state, id))
          }
        />
        {graph.render(editor, {
          structureLocked: !documentEditable,
          onSelect: (id) =>
            setEditor((state) => selectWorkflowItem(state, id)),
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
          onAdd: (type, position) =>
            setEditor((state) => addWorkflowNode(state, type, position)),
          onDelete: (ids) =>
            setEditor((state) => deleteWorkflowItems(state, new Set(ids))),
        })}
        <WorkflowPropertiesPane
          compatibility={compatibility}
          document={editor.document}
          editable={documentEditable}
          issues={issues}
          selectedEdge={selectedEdge}
          selectedId={selectedId}
          selectedNode={selectedNode}
          summary={summary}
          onDelete={() => setEditor(deleteSelectedWorkflowItems)}
          onEdgeFields={(patch) =>
            setEditor((state) => updateSelectedEdgeFields(state, patch))
          }
          onNodeField={(key, value) =>
            setEditor((state) => updateSelectedNodeField(state, key, value))
          }
          onNodeProperty={(key, value) =>
            setEditor((state) =>
              updateSelectedNodeProperty(state, key, value),
            )
          }
          onOpenSettings={onOpenSettings}
          onPendingDraftChange={setPendingPropertyDraft}
          onRenameNode={(currentId, nextId) =>
            setEditor((state) => renameWorkflowNode(state, currentId, nextId))
          }
          onSelectIssue={(id) =>
            setEditor((state) => selectWorkflowItem(state, id))
          }
          onWorkflowField={(key, value) =>
            setEditor((state) =>
              editWorkflow(state, (current) => ({
                ...current,
                [key]: value,
              })),
            )
          }
        />
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
  return selectWorkflowItem(
    editor,
    typeof id === "string" ? id : `node-${missing}`,
  );
}

function fileSafeName(name: string): string {
  const normalized = name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "");
  return normalized === "" ? "workflow" : normalized;
}

function readFile(file: File): Promise<string> {
  if (typeof file.text === "function") return file.text();
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(reader.error ?? new Error("File read failed"));
    reader.onload = () => resolve(String(reader.result ?? ""));
    reader.readAsText(file);
  });
}

function saveTitleFor(
  blockingIssueCount: number,
  editable: boolean,
  pendingPropertyDraft: boolean,
  dirty: boolean,
): string {
  if (!editable)
    return "This stored or future workflow schema is inspectable and exportable but read-only";
  if (blockingIssueCount > 0)
    return "Resolve structural validation errors before saving";
  if (pendingPropertyDraft)
    return "Apply or discard the pending node ID or configuration draft before saving";
  if (!dirty) return "No unsaved workflow changes";
  return "Save this workflow with optimistic version checking";
}

function runTitleFor(
  editable: boolean,
  executable: boolean,
  issueCount: number,
  pendingPropertyDraft: boolean,
  dirty: boolean,
  runAvailable: boolean,
): string {
  if (!editable)
    return "This stored or future workflow schema is inspectable and exportable but read-only";
  if (!executable)
    return "Native Run currently supports only the exact Simple Chat graph";
  if (issueCount > 0)
    return "Resolve every dependency and document validation issue before running";
  if (pendingPropertyDraft)
    return "Apply or discard the pending node property draft before running";
  if (dirty) return "Save this workflow before starting a Run";
  if (!runAvailable) return "No Run action is connected to this editor";
  return "Open a new Chat with this saved workflow selected";
}
