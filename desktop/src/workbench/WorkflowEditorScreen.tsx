import { useEffect, useMemo, useRef, useState } from "react";
import { useProjectedNotification } from "../notifications/NotificationContext";
import { useNotificationMessage } from "../notifications/useNotificationMessage";
import type { SettingsV2Snapshot } from "./configuration";
import { createSettingsV2CorePort } from "./settingsV2Port";
import { portableMcpDocument } from "./mcpReferences";
import {
  createWorkflowCorePort,
  nextWorkbenchCommandId,
  type WorkflowCorePort,
  type WorkflowCreateReceipt,
  type WorkflowLibraryPort,
  type WorkflowLibrarySnapshot,
  type WorkflowSnapshot,
} from "./corePort";
import { WorkflowGraphSurfaceAdapter } from "./graphSurface";
import { WorkflowLibraryBar } from "./WorkflowLibraryBar";
import { WorkflowNameDialog } from "./WorkflowNameDialog";
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
  /** Native Import/Export file handling; tests inject their own. */
  readonly filePort?: WorkflowFilePort;
  readonly settings?: SettingsV2Snapshot;
  readonly runFacts?: readonly NodeRunFact[];
  readonly onOpenSettings?: () => void;
  /** Reports that the stored workflow library changed, so other surfaces reload it. */
  readonly onLibraryChange?: () => void;
}

/** Which text-input dialog is open: naming a new workflow, or Saving As. */
type WorkflowNameDialogKind = "new" | "save-as";

/** Lossless visual document editor for the workflows in the workflow folder. */
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
  const [nameDialog, setNameDialog] = useState<WorkflowNameDialogKind | null>(
    null,
  );
  const [nameDialogError, setNameDialogError] = useState<string | null>(null);
  const [nameDialogBusy, setNameDialogBusy] = useState(false);
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
  /** Every control is unavailable while another workflow operation runs. */
  const controlsBusy = saving || fileBusy || libraryBusy || nameDialogBusy;
  const saveAsDisabled =
    !documentEditable ||
    saveBlockingIssues.length > 0 ||
    pendingPropertyDraft ||
    controlsBusy;
  const notificationScope = `workflow:${activeWorkflowId ?? "unloaded"}`;
  useProjectedNotification("Workflows", notificationScope, "error", error === null ? null : {
    route: "workflows", summary: error, detail: "The complete local document remains available for Undo or Save As.", severity: "error", lifetime: { kind: "transient" },
  }, true, errorOccurrence);
  useProjectedNotification("Workflow library", notificationScope, "library-error", libraryError === null ? null : {
    route: "workflows", summary: libraryError, severity: "error", lifetime: { kind: "transient" },
  }, true, libraryErrorOccurrence);
  useProjectedNotification("Workflows", notificationScope, "notice", error !== null || notice === null ? null : {
    route: "workflows", summary: notice, severity: "success", lifetime: { kind: "transient" },
  }, true, noticeOccurrence);
  useProjectedNotification("Workflows", notificationScope, "saving", !saving && !libraryBusy && !fileBusy && !nameDialogBusy ? null : {
    route: "workflows",
    summary: saving
      ? "Saving workflow…"
      : nameDialogBusy
        ? "Storing the workflow in the workflow folder…"
        : libraryBusy
          ? "Updating the workflow folder…"
          : "Reading or writing a workflow file…",
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
   * workflow library uses. The pending command ID is reused by every retry.
   * Returns the core's save-time notice for a successful save (null means the
   * save was refused and the error is already shown). */
  const commitDocument = async (
    document: WorkflowDocument,
  ): Promise<string | null> => {
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
        return null;
      }
      setProjectedVersion(receipt.currentVersion);
      setStoredEditable(true);
      setRetryCommandId(null);
      // The core always stores what the editor kept losslessly, and its verdict
      // then names any reason this document cannot start a Chat, so the reason
      // is surfaced at save time instead of at the next send.
      return receipt.reason ?? "";
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
      return null;
    }
  };

  /**
   * Save writes the open workflow back to the same workflow JSON file it was
   * loaded from: the workflow folder entry this editor edits. It opens no file
   * dialog, and the active workflow and every dropdown stay as they are.
   */
  const save = async (): Promise<void> => {
    if (
      saveBlockingIssues.length > 0 ||
      !documentEditable ||
      pendingPropertyDraft ||
      !dirty ||
      saving
    )
      return;
    setSaving(true);
    setError(null);
    setNotice(null);
    try {
      const saveNotice = await commitDocument(editor.document);
      if (saveNotice === null) return;
      setSavedFingerprint(serializeWorkflow(editor.document));
      setNotice(saveNotice === "" ? `Saved ${workflowName}.` : saveNotice);
      await refreshLibraryQuietly();
    } finally {
      setSaving(false);
    }
  };

  /** Opens the one text-input dialog; Save As and New differ in what confirm does. */
  const openNameDialog = (kind: WorkflowNameDialogKind): void => {
    if (controlsBusy) return;
    setNameDialogError(null);
    setNameDialog(kind);
  };

  const closeNameDialog = (): void => {
    if (nameDialogBusy) return;
    setNameDialog(null);
    setNameDialogError(null);
  };

  /**
   * Stores the entered name in the workflow folder. Save As stores the open
   * document as a new workflow; New stores a blank one. Either becomes the
   * active workflow in this editor and appears in every workflow dropdown. A
   * name another workflow already shows is refused with the dialog still open,
   * so nothing is overwritten and nothing the user typed is lost.
   */
  const confirmNameDialog = async (name: string): Promise<void> => {
    if (libraryPort === undefined || nameDialog === null) return;
    const kind = nameDialog;
    setNameDialogBusy(true);
    setNameDialogError(null);
    let receipt: WorkflowCreateReceipt;
    try {
      receipt =
        kind === "save-as"
          ? await libraryPort.saveAs({
              commandId: nextWorkbenchCommandId("workflow"),
              name,
              document: editor.document,
            })
          : await libraryPort.create({
              commandId: nextWorkbenchCommandId("workflow"),
              name,
            });
    } catch (failure) {
      // The dialog stays open, so the name can be corrected without losing
      // what was typed, and nothing was stored.
      setNameDialogError(failureMessageOf(failure));
      setNameDialogBusy(false);
      return;
    }
    setNameDialog(null);
    setNameDialogBusy(false);
    setActiveWorkflowId(receipt.workflowId);
    setNotice(
      kind === "save-as"
        ? `Saved ${name} as a new workflow in the workflow folder.`
        : `Created the blank workflow ${name} in the workflow folder.`,
    );
    // The accepted create opens its workflow even if the follow-up read of the
    // folder fails; that failure is reported, never silently ignored.
    await refreshLibraryQuietly();
  };

  /**
   * New asks for a name and then creates a blank workflow JSON document in the
   * workflow folder that becomes the active workflow in this editor.
   */
  const newWorkflow = async (): Promise<void> => {
    if (controlsBusy || !documentEditable) return;
    if (
      dirty &&
      !(await requestDiscard(
        "Start a new workflow?",
        `Starting a new workflow discards the unsaved changes of ${workflowName}.`,
      ))
    )
      return;
    openNameDialog("new");
  };

  /**
   * Import reads one workflow document from any location through the operating
   * system's own file dialog, validates it first, and only then copies it into
   * the workflow folder as a new workflow that becomes the active one. An
   * unreadable, unparseable, invalid, or already-taken-name file leaves the
   * editor, the workflow folder and every dropdown exactly as they were.
   */
  const importWorkflow = async (): Promise<void> => {
    if (controlsBusy || !documentEditable || libraryPort === undefined) return;
    if (
      dirty &&
      !(await requestDiscard(
        "Import another workflow?",
        `Importing another workflow discards the unsaved changes of ${workflowName}.`,
      ))
    )
      return;
    setFileBusy(true);
    setError(null);
    setNotice(null);
    try {
      const path = await files.chooseImportPath();
      if (path === null) return;
      const loaded = await readWorkflowFile(files, path);
      const refusal = storeRefusal(loaded);
      if (refusal !== null) {
        setError(`Import failed: ${refusal}`);
        return;
      }
      const name = workflowNameOf(loaded, path);
      const receipt = await libraryPort.saveAs({
        commandId: nextWorkbenchCommandId("workflow"),
        name,
        document: loaded,
      });
      setActiveWorkflowId(receipt.workflowId);
      setNotice(
        `Imported ${workflowFileNameFromPath(path)} as ${name} in the workflow folder.`,
      );
      await refreshLibraryQuietly();
    } catch (failure) {
      setError(`Import failed: ${failureMessageOf(failure)}`);
    } finally {
      setFileBusy(false);
    }
  };

  /**
   * Export writes the open workflow document to any path the user chooses in the
   * operating system's save dialog. It is a copy: the active workflow, the
   * workflow folder and every dropdown stay exactly as they were.
   */
  const exportWorkflow = async (): Promise<void> => {
    if (controlsBusy) return;
    let path: string | null;
    try {
      path = await files.chooseExportPath(
        suggestedWorkflowFileName(workflowName),
      );
    } catch (failure) {
      setError(`Export failed: ${failureMessageOf(failure)}`);
      return;
    }
    if (path === null) return;
    setFileBusy(true);
    setError(null);
    setNotice(null);
    try {
      const contents = JSON.stringify(portableMcpDocument(editor.document, settings?.settings.mcpServers ?? []), null, 2);
      const first = await files.writeDocument({
        path,
        contents,
        overwrite: false,
      });
      if (first === "exists") {
        const replace = await files.confirm(
          "Replace existing workflow file?",
          `${path} already exists. Replacing it overwrites the document that file holds.`,
        );
        if (!replace) {
          setNotice(`Left ${workflowFileNameFromPath(path)} unchanged.`);
          return;
        }
        await files.writeDocument({ path, contents, overwrite: true });
      }
      setNotice(
        `Exported ${workflowName} to ${workflowFileNameFromPath(path)}. The workflow folder is unchanged.`,
      );
    } catch (failure) {
      setError(`Export failed: ${failureMessageOf(failure)}`);
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

  const deleteWorkflow = (workflowId: string): void => {
    void runLibraryAction(async () => {
      await libraryPort!.remove({
        commandId: nextWorkbenchCommandId("workflow"),
        workflowId,
      });
      // The deleted workflow was never the default one, so the default is
      // always a safe document to select next.
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
        canRedo={editor.redo.length > 0}
        canUndo={editor.undo.length > 0}
        draftSaved={!dirty}
        editable={documentEditable}
        executable={compatibility.executable}
        exportDisabled={controlsBusy}
        exportTitle={exportTitle(controlsBusy)}
        importDisabled={!documentEditable || controlsBusy || libraryPort === undefined}
        importTitle={importTitle(documentEditable, libraryPort !== undefined)}
        newDisabled={!documentEditable || controlsBusy || libraryPort === undefined}
        newTitle={newTitle(documentEditable, libraryPort !== undefined)}
        projectedVersion={projectedVersion}
        saveAsDisabled={saveAsDisabled || libraryPort === undefined}
        saveAsTitle={saveAsTitleFor(
          saveBlockingIssues.length,
          documentEditable,
          pendingPropertyDraft,
          controlsBusy || libraryPort === undefined,
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
        onExport={() => void exportWorkflow()}
        onImport={() => void importWorkflow()}
        onNew={() => void newWorkflow()}
        onRedo={() => setEditor(redoWorkflow)}
        onSave={() => void save()}
        onSaveAs={() => openNameDialog("save-as")}
        onUndo={() => setEditor(undoWorkflow)}
        onValidate={selectValidationResult}
      />
      {libraryPort !== undefined && library !== null && activeWorkflowId !== null && (
        <WorkflowLibraryBar
          activeWorkflowId={activeWorkflowId}
          busy={libraryBusy}
          library={library}
          onDelete={deleteWorkflow}
          onSelect={setActiveWorkflowId}
          onSetDefault={setDefaultWorkflow}
        />
      )}
      {nameDialog !== null && (
        <WorkflowNameDialog
          busy={nameDialogBusy}
          confirmLabel={nameDialog === "save-as" ? "Save copy" : "Create"}
          error={nameDialogError}
          initialName={nameDialog === "save-as" ? workflowName : undefined}
          inputLabel={
            nameDialog === "save-as" ? "Save as workflow name" : "New workflow name"
          }
          title={
            nameDialog === "save-as"
              ? "Save this workflow under a new name"
              : "Name the new workflow"
          }
          onCancel={closeNameDialog}
          onConfirm={(name) => void confirmNameDialog(name)}
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
 * The name an imported workflow is stored under: a document that carries its own
 * name keeps it, and a file basename is only the fallback for one that does not.
 * Nothing in the workflow folder ever depends on the two matching.
 */
function workflowNameOf(document: WorkflowDocument, path: string): string {
  const name = document.name;
  if (typeof name === "string" && name.trim() !== "") return name.trim();
  return workflowNameFromPath(path);
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
 * Reasons this build will not store a document in the workflow folder. The same
 * rule gates Save, so a file that could never be stored is refused before the
 * folder adopts it — an invalid import changes nothing at all.
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
  unavailable: boolean,
): string {
  if (!editable)
    return "This stored or future workflow schema is inspectable but read-only; Save and Save As would overwrite it";
  if (blockingIssueCount > 0)
    return "Resolve structural validation errors before saving a copy";
  if (pendingPropertyDraft)
    return "Apply or discard the pending node ID or configuration draft before saving a copy";
  if (unavailable) return "A workflow operation is already in progress";
  return "Ask for a name and save a copy as a new workflow in the workflow folder";
}

function newTitle(editable: boolean, libraryAvailable: boolean): string {
  if (!editable)
    return "This stored or future workflow schema is read-only and cannot be replaced by a new document";
  if (!libraryAvailable)
    return "Creating a workflow needs the native workflow folder";
  return "Ask for a name and create a blank workflow in the workflow folder, asking about unsaved changes first";
}

function importTitle(editable: boolean, libraryAvailable: boolean): string {
  if (!editable)
    return "This stored or future workflow schema is read-only and cannot be replaced by an imported file";
  if (!libraryAvailable)
    return "Importing a workflow needs the native workflow folder";
  return "Validate a workflow JSON document chosen in the operating system's own file chooser and copy it into the workflow folder";
}

function exportTitle(busy: boolean): string {
  return busy
    ? "A workflow operation is already in progress"
    : "Write this workflow JSON document to any location chosen in the operating system's own file chooser";
}
