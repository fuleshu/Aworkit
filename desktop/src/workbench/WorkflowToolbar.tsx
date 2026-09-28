interface WorkflowToolbarProps {
  readonly workflowName: string;
  readonly projectedVersion: number;
  readonly editable: boolean;
  readonly executable: boolean;
  readonly validationCount: number;
  /** Whether the editor and the stored workflow hold the same document. */
  readonly draftSaved: boolean;
  readonly canUndo: boolean;
  readonly canRedo: boolean;
  readonly saving: boolean;
  readonly newDisabled: boolean;
  readonly newTitle: string;
  readonly importDisabled: boolean;
  readonly importTitle: string;
  readonly saveDisabled: boolean;
  readonly saveTitle: string;
  readonly saveAsDisabled: boolean;
  readonly saveAsTitle: string;
  readonly exportDisabled: boolean;
  readonly exportTitle: string;
  readonly onNew: () => void;
  readonly onImport: () => void;
  readonly onSave: () => void;
  readonly onSaveAs: () => void;
  readonly onExport: () => void;
  readonly onUndo: () => void;
  readonly onRedo: () => void;
  readonly onValidate: () => void;
}

/**
 * The Workflow designer's document commands.
 *
 * New, Save As and Import name or store a workflow inside the Aworkit workflow
 * folder, so they never ask for a path; Import and Export are the only commands
 * that open an operating-system file dialog. This surface has no Run control: it
 * writes workflow documents and never starts a Run or installs node
 * implementations.
 */
export function WorkflowToolbar({
  workflowName,
  projectedVersion,
  editable,
  executable,
  validationCount,
  draftSaved,
  canUndo,
  canRedo,
  saving,
  newDisabled,
  newTitle,
  importDisabled,
  importTitle,
  saveDisabled,
  saveTitle,
  saveAsDisabled,
  saveAsTitle,
  exportDisabled,
  exportTitle,
  onNew,
  onImport,
  onSave,
  onSaveAs,
  onExport,
  onUndo,
  onRedo,
  onValidate,
}: WorkflowToolbarProps): React.JSX.Element {
  return (
    <header className="surface-toolbar">
      <div>
        <p className="eyebrow">WORKFLOW</p>
        <h1>{workflowName}</h1>
      </div>
      <div className="toolbar-actions">
        <span>Version {projectedVersion}</span>
        <span
          className={`status ${editable && executable ? "ready" : "unconfigured"}`}
        >
          {!editable
            ? "Read-only schema"
            : executable
              ? "Executable workflow"
              : "Editable · Not runnable"}
        </span>
        <span
          className={`status ${draftSaved ? "ready" : "unconfigured"}`}
          title={
            draftSaved
              ? "The stored workflow and this editor hold the same document"
              : "The editor holds changes the workflow folder has not accepted yet"
          }
        >
          {draftSaved ? "✓ Draft saved" : "Unsaved changes"}
        </span>
        <button
          disabled={newDisabled}
          title={newTitle}
          type="button"
          onClick={onNew}
        >
          New
        </button>
        <button
          disabled={importDisabled}
          title={importTitle}
          type="button"
          onClick={onImport}
        >
          Import
        </button>
        <button
          className="primary-action"
          disabled={saveDisabled}
          title={saveTitle}
          type="button"
          onClick={onSave}
        >
          {saving ? "Saving…" : "Save"}
        </button>
        <button
          disabled={saveAsDisabled}
          title={saveAsTitle}
          type="button"
          onClick={onSaveAs}
        >
          Save As
        </button>
        <button
          disabled={exportDisabled}
          title={exportTitle}
          type="button"
          onClick={onExport}
        >
          Export
        </button>
        <button
          disabled={!canUndo}
          title="Undo the last workflow transaction"
          type="button"
          onClick={onUndo}
        >
          ↶ Undo
        </button>
        <button
          disabled={!canRedo}
          title="Redo the last undone transaction"
          type="button"
          onClick={onRedo}
        >
          ↷ Redo
        </button>
        <button
          title="Validate document integrity, dependencies, and native executability"
          type="button"
          onClick={onValidate}
        >
          Validate{" "}
          <span
            className={validationCount > 0 ? "count-warning" : "count-success"}
          >
            {validationCount}
          </span>
        </button>
      </div>
    </header>
  );
}
