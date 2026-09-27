import { useEffect, useMemo, useRef, useState } from "react";
import { useProjectedNotification } from "../notifications/NotificationContext";
import { useNotificationMessage } from "../notifications/useNotificationMessage";
import {
  bundledCreationDefaultTemplateId,
  bundledWorkflowTemplates,
} from "./bundledWorkflows";
import type { SettingsV2Snapshot } from "./configuration";
import { createSettingsV2CorePort } from "./settingsV2Port";
import {
  createWorkflowCorePort,
  createWorkflowLibraryPort,
  nextWorkbenchCommandId,
  type WorkflowCorePort,
  type WorkflowLibraryPort,
  type WorkflowLibrarySnapshot,
  type WorkflowSnapshot,
} from "./corePort";
import { WorkflowGraphSurfaceAdapter } from "./graphSurface";
import { WorkflowLibraryBar } from "./WorkflowLibraryBar";
import { WorkflowPalette } from "./WorkflowPalette";
import { WorkflowPropertiesPane } from "./WorkflowPropertiesPane";
import { WorkflowToolbar } from "./WorkflowToolbar";
import {
  createWorkflowFilePort,
  suggestedWorkflowFileName,
  workflowFileNameFromPath,
  workflowNameFromPath,
  type WorkflowFilePort,
} from "./workflowFilePort";
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
  selectedWorkflowEdge,
  selectedWorkflowNode,
  selectWorkflowItem,
  serializeWorkflow,
  undoWorkflow,
  updateSelectedEdgeFields,
  updateSelectedNodeConfiguration,
  updateSelectedNodeField,
  updateSelectedNodeProperty,
  validateWorkflow,
  workflowSummary,
  type WorkflowDocument,
} from "./workflow";
import { assessNativeWorkflow } from "./workflowExecution";
import { projectNodeRunStatus, type NodeRunFact } from "./runStatus";

interface WorkflowEditorScreenProps {
  readonly active?: boolean;
  readonly document: WorkflowDocument;
  readonly workflowPort?: WorkflowCorePort;
  readonly libraryPort?: WorkflowLibraryPort;
  /** Native New/Open/Save/Save As file handling; tests inject their own. */
  readonly filePort?: WorkflowFilePort;
  readonly settings?: SettingsV2Snapshot;
  readonly runFacts?: readonly NodeRunFact[];
  readonly onOpenSettings?: () => void;
  /** Reports that the stored workflow library changed, so other surfaces reload it. */
  readonly onLibraryChange?: () => void;
}

/**
 * One workflow file belongs to the workflow library entry its document was
 * loaded into, so switching to another workflow can never write one entry's
 * document over another entry's file.
 */
interface WorkflowFileBinding {
  readonly workflowId: string | null;
  readonly path: string;
}

/** Lossless visual document editor with standard New/Open/Save/Save As handling. */
export function WorkflowEditorScreen({
  document,
  workflowPort,
  libraryPort,
  filePort,
  settings: settingsProp,
  runFacts,
  onOpenSettings,
  onLibraryChange,
  active = true,
}: WorkflowEditorScreenProps): React.JSX.Element {
  const port = useMemo(
    () => workflowPort ?? createWorkflowCorePort(document),
    [document, workflowPort],
  );
  const files = useMemo(() => filePort ?? createWorkflowFilePort(), [filePort]);
  const [settings, setSettings] = useState<SettingsV2Snapshot | undefined>(
    settingsProp,
  );
  useEffect(() => {
    if (settingsProp !== undefined || !active || !("__TAURI_INTERNALS__" in window)) return;
    let current = true;
    void createSettingsV2CorePort()
      .snapshot()
      .then((snapshot) => {
        if (current) setSettings(snapshot);
      })
      .catch(() => {
        // The typed property forms fall back to standard tiers without Settings.
      });
    return () => {
      current = false;
    };
  }, [settingsProp, active]);
  const [editor, setEditor] = useState(() => createSelectedEditor(document));
  const [savedFingerprint, setSavedFingerprint] = useState(() =>
    serializeWorkflow(document),
  );
  const [projectedVersion, setProjectedVersion] = useState(0);
  const [storedEditable, setStoredEditable] = useState(
    document.schemaVersion === 1,
  );
  const [storedDocumentId, setStoredDocumentId] = useState<string | null>(
    typeof document.id === "string" ? document.id : null,
  );
  const [fileBinding, setFileBinding] = useState<WorkflowFileBinding | null>(
    null,
  );
  const [fileBusy, setFileBusy] = useState(false);
  const [saving, setSaving] = useState(false);
  const [pendingPropertyDraft, setPendingPropertyDraft] = useState(false);
  const [error, setError, errorOccurrence] = useNotificationMessage();
  const [notice, setNotice, noticeOccurrence] = useNotificationMessage();
  const [retryCommandId, setRetryCommandId] = useState<string | null>(null);
  const [library, setLibrary] = useState<WorkflowLibrarySnapshot | null>(null);
  const [activeWorkflowId, setActiveWorkflowId] = useState<string | null>(null);
  const [libraryBusy, setLibraryBusy] = useState(false);
  const [libraryError, setLibraryError, libraryErrorOccurrence] = useNotificationMessage();
  const graph = useMemo(() => new WorkflowGraphSurfaceAdapter(), []);

  /** Applies a freshly loaded workflow snapshot to the editor surface. */
  const applySnapshot = (snapshot: WorkflowSnapshot): void => {
    setProjectedVersion(snapshot.version);
    setStoredEditable(snapshot.editable);
    setStoredDocumentId(
      typeof snapshot.document.id === "string" ? snapshot.document.id : null,
    );
    setEditor(createSelectedEditor(snapshot.document));
    setSavedFingerprint(serializeWorkflow(snapshot.document));
    setError(null);
  };

  useEffect(() => {
    if (libraryPort !== undefined) return;
    let active = true;
    void port
      .snapshot()
      .then((snapshot) => {
        if (active) applySnapshot(snapshot);
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
  }, [port, libraryPort]);

  useEffect(() => {
    if (libraryPort === undefined) return;
    let active = true;
    setLibraryError(null);
    void libraryPort
      .snapshot()
      .then((snapshot) => {
        if (!active) return;
        setLibrary(snapshot);
        setActiveWorkflowId((current) => current ?? snapshot.defaultWorkflowId);
      })
      .catch((failure: unknown) => {
        if (active)
          setLibraryError(
            failure instanceof Error ? failure.message : String(failure),
          );
      });
    return () => {
      active = false;
    };
  }, [libraryPort]);

  useEffect(() => {
    if (libraryPort === undefined || activeWorkflowId === null) return;
    let current = true;
    void port
      .snapshot(activeWorkflowId)
      .then((snapshot) => {
        if (current) applySnapshot(snapshot);
      })
      .catch((failure: unknown) => {
        if (current)
          setError(
            failure instanceof Error ? failure.message : String(failure),
          );
      });
    return () => {
      current = false;
    };
  }, [port, libraryPort, activeWorkflowId]);

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
  /** The workflow library entry this editor edits; it is the document identity. */
  const documentIdentity = activeWorkflowId ?? storedDocumentId;
  const boundFilePath =
    fileBinding !== null && fileBinding.workflowId === activeWorkflowId
      ? fileBinding.path
      : null;
  const fileControlsDisabled = saving || fileBusy;
  const saveAsDisabled =
    !documentEditable ||
    saveBlockingIssues.length > 0 ||
    pendingPropertyDraft ||
    fileControlsDisabled;
  const notificationScope = `workflow:${activeWorkflowId ?? "draft"}`;
  useProjectedNotification("Workflows", notificationScope, "error", error === null ? null : {
    route: "workflows", summary: error, detail: "The complete local document remains available for Undo or Save As.", severity: "error", lifetime: { kind: "transient" },
  }, true, errorOccurrence);
  useProjectedNotification("Workflow library", notificationScope, "library-error", libraryError === null ? null : {
    route: "workflows", summary: libraryError, severity: "error", lifetime: { kind: "transient" },
  }, true, libraryErrorOccurrence);
  useProjectedNotification("Workflows", notificationScope, "notice", error !== null || notice === null ? null : {
    route: "workflows", summary: notice, severity: "success", lifetime: { kind: "transient" },
  }, true, noticeOccurrence);
  useProjectedNotification("Workflows", notificationScope, "saving", !saving && !libraryBusy && !fileBusy ? null : {
    route: "workflows",
    summary: saving
      ? "Saving workflow…"
      : libraryBusy
        ? "Updating workflow library…"
        : "Reading workflow file…",
    severity: "progress",
    lifetime: { kind: "operation", operationId: retryCommandId ?? "library" },
  });
  useProjectedNotification("Workflows", notificationScope, "dependency", missingIssue === undefined ? null : {
    route: "workflows", summary: `Missing dependency: ${missingIssue.message}`, severity: "warning", lifetime: { kind: "condition", conditionId: `${activeWorkflowId}:${missingIssue.itemId}` },
    action: { label: "Inspect dependency", run: () => setEditor(state => selectWorkflowItem(state, missingIssue.itemId)) },
  }, active);
  useProjectedNotification("Workflows", notificationScope, "compatibility", documentEditable && compatibility.executable ? null : {
    route: "workflows", severity: "info", lifetime: { kind: "condition", conditionId: "workflow-compatibility" },
    summary: !documentEditable ? "Read-only workflow document." : "Editable document; native execution is limited.",
    detail: !documentEditable ? "This stored or future schema is preserved for inspection and a lossless Save As; this build will not overwrite it." : compatibility.issues[0]?.message,
  }, active);
  const previousFeedback = useRef({ fingerprint, noticeOccurrence, errorOccurrence });
  useEffect(() => {
    const previous = previousFeedback.current;
    if (previous.fingerprint !== fingerprint) {
      // Preserve feedback produced by the same transaction (for example Open).
      if (previous.noticeOccurrence === noticeOccurrence) setNotice(null);
      if (previous.errorOccurrence === errorOccurrence) setError(null);
    }
    previousFeedback.current = { fingerprint, noticeOccurrence, errorOccurrence };
  }, [fingerprint, noticeOccurrence, errorOccurrence, setNotice, setError]);
  useEffect(() => { if (!active) { setNotice(null); setError(null); setLibraryError(null); } }, [active]);
  const validationCount =
    issues.length + (compatibility.executable ? 0 : compatibility.issues.length);
  const workflowName =
    typeof editor.document.name === "string"
      ? editor.document.name
      : "Untitled workflow";

  /** Commits one complete document through the same core-accepted save the
   * workflow library uses. The pending command ID is reused by every retry. */
  const commitDocument = async (
    document: WorkflowDocument,
  ): Promise<boolean> => {
    const commandId = retryCommandId ?? nextWorkbenchCommandId("workflow");
    setRetryCommandId(commandId);
    try {
      const receipt = await port.commit({
        commandId,
        expectedVersion: projectedVersion,
        document,
        workflowId: activeWorkflowId ?? undefined,
      });
      if (!receipt.accepted) {
        setRetryCommandId(null);
        setError(receipt.reason ?? "The trusted core rejected the workflow.");
        return false;
      }
      setProjectedVersion(receipt.currentVersion);
      setStoredEditable(true);
      setRetryCommandId(null);
      return true;
    } catch (failure) {
      const failureMessage =
        failure instanceof Error ? failure.message : String(failure);
      setError(failureMessage);
      if (failureMessage.includes("version conflict")) {
        setRetryCommandId(null);
        try {
          setProjectedVersion((await port.snapshot()).version);
        } catch {
          // The complete local document stays available for Save As or retry.
        }
      }
      return false;
    }
  };

  /**
   * Writes one complete document to a file and, once it is written, commits it
   * through the same core-accepted save the workflow library uses, so a file
   * never becomes a second source of truth.
   *
   * A `"chosen"` path was chosen in this operation (Save As, or Save with no
   * file bound), so an existing file is replaced only after the user confirmed
   * it. The file is written before the commit, so declining the replacement
   * leaves the workflow library unchanged as well. A `"bound"` path is the file
   * Save already writes, and replacing it is what Save means.
   *
   * A library rejection after a successful write is reported as exactly that,
   * and no path is bound.
   */
  const persistDocument = async (
    document: WorkflowDocument,
    path: string,
    mode: "bound" | "chosen",
  ): Promise<void> => {
    if (saving) return;
    setSaving(true);
    setError(null);
    setNotice(null);
    try {
      const contents = JSON.stringify(document, null, 2);
      if (mode === "chosen") {
        const first = await files.writeDocument({
          path,
          contents,
          overwrite: false,
        });
        if (first === "exists") {
          const replace = await files.confirm(
            "Replace existing workflow file?",
            `${path} already exists. Replacing it overwrites the workflow document that file holds.`,
          );
          if (!replace) {
            setNotice(
              `Left ${workflowFileNameFromPath(path)} unchanged; the workflow library was not changed either.`,
            );
            return;
          }
          await files.writeDocument({ path, contents, overwrite: true });
        }
      } else {
        await files.writeDocument({ path, contents, overwrite: true });
      }
      if (!(await commitDocument(document))) return;
      setSavedFingerprint(serializeWorkflow(document));
      if (mode === "chosen")
        setFileBinding({ workflowId: activeWorkflowId, path });
      setNotice(
        `Saved ${workflowFileNameFromPath(path)}. The file is a copy: the workflow library remains the one canonical store.`,
      );
      await refreshLibraryQuietly();
    } catch (failure) {
      setError(
        `${workflowFileNameFromPath(path)} could not be written: ${failureMessageOf(failure)}`,
      );
    } finally {
      setSaving(false);
    }
  };

  const save = async (): Promise<void> => {
    if (
      saveBlockingIssues.length > 0 ||
      !documentEditable ||
      pendingPropertyDraft ||
      !dirty ||
      saving
    )
      return;
    // Save writes the bound file; with no file bound it is exactly Save As.
    if (boundFilePath === null) {
      await saveAs();
      return;
    }
    await persistDocument(editor.document, boundFilePath, "bound");
  };

  const saveAs = async (): Promise<void> => {
    if (saveAsDisabled) return;
    let path: string | null;
    try {
      path = await files.chooseSavePath(
        boundFilePath === null
          ? suggestedWorkflowFileName(workflowName)
          : workflowFileNameFromPath(boundFilePath),
      );
    } catch (failure) {
      setError(`Save As failed: ${failureMessageOf(failure)}`);
      return;
    }
    if (path === null) return;
    await persistDocument(editor.document, path, "chosen");
  };

  /**
   * Starts a new workflow document as an unsaved draft. The draft has no bound
   * file, so its first Save asks for a path, and nothing is stored until the
   * core accepts it.
   */
  const newDocument = async (): Promise<void> => {
    if (fileControlsDisabled) return;
    if (dirty && !(await requestDiscard(
      "Start a new workflow?",
      `Starting a new workflow discards the unsaved changes of ${workflowName}.`,
    )))
      return;
    const template = bundledWorkflowTemplates.find(
      ({ templateId }) => templateId === bundledCreationDefaultTemplateId,
    );
    if (template === undefined) {
      setError("No bundled workflow template is available for a new document.");
      return;
    }
    const draft = bindToLibraryIdentity(
      parseWorkflow(JSON.stringify(template.document)),
      documentIdentity,
    );
    setEditor(createSelectedEditor(draft));
    setSavedFingerprint(serializeWorkflow(editor.document));
    setFileBinding(null);
    setError(null);
    setNotice(
      `Started a new ${template.name} draft. It is not stored yet: Save asks for a path and keeps the workflow library canonical.`,
    );
  };

  /**
   * Opens one workflow file through the operating system's dialog, parses and
   * validates it, and activates it only as the core-accepted save accepts it.
   * An unreadable, unparseable, or unstorable file leaves the editor and the
   * workflow library exactly as they were.
   */
  const openDocument = async (): Promise<void> => {
    if (fileControlsDisabled) return;
    if (dirty && !(await requestDiscard(
      "Open another workflow?",
      `Opening another workflow document discards the unsaved changes of ${workflowName}.`,
    )))
      return;
    setFileBusy(true);
    try {
      const path = await files.chooseOpenPath();
      if (path === null) return;
      const loaded = await readWorkflowFile(files, path);
      const refusal = storeRefusal(loaded);
      if (refusal !== null) {
        setError(`Open failed: ${refusal}`);
        return;
      }
      const opened = nameFromFile(
        bindToLibraryIdentity(loaded, documentIdentity),
        path,
      );
      if (!(await commitDocument(opened))) return;
      setEditor(createSelectedEditor(opened));
      setSavedFingerprint(serializeWorkflow(opened));
      setFileBinding({ workflowId: activeWorkflowId, path });
      setNotice(
        `Opened ${workflowFileNameFromPath(path)} and stored it in the workflow library. File operations never install or enable node implementations.`,
      );
      await refreshLibraryQuietly();
    } catch (failure) {
      setError(`Open failed: ${failureMessageOf(failure)}`);
    } finally {
      setFileBusy(false);
    }
  };

  /**
   * Asks through the application's own confirmation dialog. A question that
   * cannot be asked keeps the current document, never discards it.
   */
  const requestDiscard = async (
    title: string,
    body: string,
  ): Promise<boolean> => {
    try {
      return await files.confirm(title, body);
    } catch (failure) {
      setError(failureMessageOf(failure));
      return false;
    }
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
      setNotice("Validation passed: this workflow document is executable.");
    }
  };

  /**
   * Reloads the stored library projection. The stored document is the only
   * naming truth, so every accepted library mutation republishes the entries
   * and tells other surfaces to reload them.
   */
  const refreshLibrary = async (): Promise<void> => {
    if (libraryPort === undefined) return;
    setLibrary(await libraryPort.snapshot());
    onLibraryChange?.();
  };

  /** Refreshes the library without turning its failure into a command failure. */
  const refreshLibraryQuietly = async (): Promise<void> => {
    try {
      await refreshLibrary();
    } catch (failure) {
      setLibraryError(
        failure instanceof Error ? failure.message : String(failure),
      );
    }
  };

  const runLibraryAction = async (action: () => Promise<void>): Promise<void> => {
    if (libraryPort === undefined) return;
    setLibraryBusy(true);
    setLibraryError(null);
    try {
      await action();
      await refreshLibrary();
    } catch (failure) {
      setLibraryError(
        failure instanceof Error ? failure.message : String(failure),
      );
    } finally {
      setLibraryBusy(false);
    }
  };

  const createWorkflow = (template: string, name: string): void => {
    if (libraryPort === undefined) return;
    setLibraryBusy(true);
    setLibraryError(null);
    void libraryPort
      .create({
        commandId: nextWorkbenchCommandId("workflow"),
        name: name.trim(),
        template,
      })
      .then(async (receipt) => {
        // The accepted create opens its workflow even if the follow-up read of
        // the library fails; the failure is reported, never silently ignored.
        setActiveWorkflowId(receipt.workflowId);
        await refreshLibrary();
      })
      .catch((failure: unknown) =>
        setLibraryError(
          failure instanceof Error ? failure.message : String(failure),
        ),
      )
      .finally(() => setLibraryBusy(false));
  };

  const duplicateWorkflow = (workflowId: string, name: string): void => {
    void runLibraryAction(async () => {
      const receipt = await libraryPort!.duplicate({
        commandId: nextWorkbenchCommandId("workflow"),
        workflowId,
        name: name.trim(),
      });
      setActiveWorkflowId(receipt.workflowId);
    });
  };

  const deleteWorkflow = (workflowId: string): void => {
    void runLibraryAction(async () => {
      await libraryPort!.remove({
        commandId: nextWorkbenchCommandId("workflow"),
        workflowId,
      });
      const snapshot = await libraryPort!.snapshot();
      setActiveWorkflowId(snapshot.defaultWorkflowId);
    });
  };

  const setDefaultWorkflow = (workflowId: string): void => {
    void runLibraryAction(async () => {
      await libraryPort!.setDefault({
        commandId: nextWorkbenchCommandId("workflow"),
        workflowId,
      });
    });
  };

  const runStatus = useMemo(
    () => projectNodeRunStatus(editor.document, runFacts ?? []),
    [editor.document, runFacts],
  );

  return (
    <section
      className={`workflow-editor ${libraryPort !== undefined ? "with-library" : ""}`}
    >
      <WorkflowToolbar
        boundFilePath={boundFilePath}
        canRedo={editor.redo.length > 0}
        canUndo={editor.undo.length > 0}
        draftSaved={!dirty}
        editable={documentEditable}
        executable={compatibility.executable}
        newDisabled={!documentEditable || fileControlsDisabled}
        newTitle={newDocumentTitle(documentEditable)}
        openDisabled={!documentEditable || fileControlsDisabled}
        openTitle={openDocumentTitle(documentEditable)}
        projectedVersion={projectedVersion}
        saveAsDisabled={saveAsDisabled}
        saveAsTitle={saveAsTitleFor(
          saveBlockingIssues.length,
          documentEditable,
          pendingPropertyDraft,
          fileControlsDisabled,
        )}
        saveDisabled={
          saving ||
          saveBlockingIssues.length > 0 ||
          !documentEditable ||
          pendingPropertyDraft ||
          !dirty
        }
        saveTitle={saveTitleFor(
          saveBlockingIssues.length,
          documentEditable,
          pendingPropertyDraft,
          dirty,
        )}
        saving={saving}
        validationCount={validationCount}
        workflowName={workflowName}
        onNew={() => void newDocument()}
        onOpen={() => void openDocument()}
        onRedo={() => setEditor(redoWorkflow)}
        onSave={() => void save()}
        onSaveAs={() => void saveAs()}
        onUndo={() => setEditor(undoWorkflow)}
        onValidate={selectValidationResult}
      />
      {libraryPort !== undefined && library !== null && activeWorkflowId !== null && (
        <WorkflowLibraryBar
          activeWorkflowId={activeWorkflowId}
          busy={libraryBusy}
          library={library}
          onCreate={createWorkflow}
          onDelete={deleteWorkflow}
          onDuplicate={duplicateWorkflow}
          onSelect={setActiveWorkflowId}
          onSetDefault={setDefaultWorkflow}
        />
      )}
      <div className="workflow-body">
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
          runStatus,
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
          settings={settings}
          summary={summary}
          onDelete={() => setEditor(deleteSelectedWorkflowItems)}
          onEdgeFields={(patch) =>
            setEditor((state) => updateSelectedEdgeFields(state, patch))
          }
          onNodeConfiguration={(patch) =>
            setEditor((state) => updateSelectedNodeConfiguration(state, patch))
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

/**
 * Rebinds a document loaded from a file or a template to the workflow library
 * entry this editor edits: the core requires a stored document to keep its
 * library identity, and the library stays the one canonical store. Every other
 * field of the loaded document — including unknown nodes, unknown fields,
 * transition positions, and requirement metadata — is kept verbatim.
 */
function bindToLibraryIdentity(
  document: WorkflowDocument,
  identity: string | null,
): WorkflowDocument {
  if (identity === null || document.id === identity) return document;
  return { ...document, id: identity };
}

/** A file's basename is only a suggestion for a document that carries no name. */
function nameFromFile(
  document: WorkflowDocument,
  path: string,
): WorkflowDocument {
  const name = document.name;
  if (typeof name === "string" && name.trim() !== "") return document;
  return { ...document, name: workflowNameFromPath(path) };
}

/** Reads and parses one chosen file, naming the file in every failure. */
async function readWorkflowFile(
  files: WorkflowFilePort,
  path: string,
): Promise<WorkflowDocument> {
  const text = await files.readDocument(path);
  try {
    return parseWorkflow(text);
  } catch (failure) {
    throw new Error(
      `${workflowFileNameFromPath(path)} is not a valid workflow JSON document: ${failureMessageOf(failure)}`,
    );
  }
}

/**
 * Reasons this build will not store a document in the workflow library. The
 * same rule gates Save, so a file that could never be stored is refused before
 * the editor adopts it instead of being lost on the next library switch.
 */
function storeRefusal(document: WorkflowDocument): string | null {
  if (document.schemaVersion !== 1)
    return `this build edits and stores schema version 1 documents; the file carries schema version ${document.schemaVersion}, which stays inspectable but is never overwritten`;
  const blocking = validateWorkflow(document).find(
    (issue) => issue.code !== "missing_dependency",
  );
  return blocking === undefined ? null : blocking.message;
}

function failureMessageOf(failure: unknown): string {
  return failure instanceof Error ? failure.message : String(failure);
}

function saveTitleFor(
  blockingIssueCount: number,
  editable: boolean,
  pendingPropertyDraft: boolean,
  dirty: boolean,
): string {
  if (!editable)
    return "This stored or future workflow schema is inspectable but read-only; Save and Save As would overwrite it";
  if (blockingIssueCount > 0)
    return "Resolve structural validation errors before saving";
  if (pendingPropertyDraft)
    return "Apply or discard the pending node ID or configuration draft before saving";
  if (!dirty) return "No unsaved workflow changes";
  return "Save this workflow with optimistic version checking";
}

function saveAsTitleFor(
  blockingIssueCount: number,
  editable: boolean,
  pendingPropertyDraft: boolean,
  fileControlsDisabled: boolean,
): string {
  if (!editable)
    return "This stored or future workflow schema is inspectable but read-only; Save and Save As would overwrite it";
  if (blockingIssueCount > 0)
    return "Resolve structural validation errors before writing a workflow file";
  if (pendingPropertyDraft)
    return "Apply or discard the pending node ID or configuration draft before writing a workflow file";
  if (fileControlsDisabled)
    return "A workflow file operation is already in progress";
  return "Choose a workflow file, write the complete JSON document, and rebind later saves to it";
}

function newDocumentTitle(editable: boolean): string {
  return editable
    ? "Start a new workflow document as an unsaved draft, asking about unsaved changes first"
    : "This stored or future workflow schema is read-only and cannot be replaced by a new document";
}

function openDocumentTitle(editable: boolean): string {
  return editable
    ? "Open a workflow JSON document through the operating system's own file chooser"
    : "This stored or future workflow schema is read-only and cannot be replaced by an opened file";
}
