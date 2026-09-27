import { workflowFileNameFromPath } from "./workflowFilePort";

interface WorkflowToolbarProps {
  readonly workflowName: string;
  readonly projectedVersion: number;
  readonly editable: boolean;
  readonly executable: boolean;
  readonly validationCount: number;
  /** The file the next Save writes, or `null` when no file is bound yet. */
  readonly boundFilePath: string | null;
  /** Whether the workflow library already holds the document the editor shows. */
  readonly draftSaved: boolean;
  readonly canUndo: boolean;
  readonly canRedo: boolean;
  readonly saving: boolean;
  readonly newDisabled: boolean;
  readonly newTitle: string;
  readonly openDisabled: boolean;
  readonly openTitle: string;
  readonly saveDisabled: boolean;
  readonly saveTitle: string;
  readonly saveAsDisabled: boolean;
  readonly saveAsTitle: string;
  readonly onNew: () => void;
  readonly onOpen: () => void;
  readonly onSave: () => void;
  readonly onSaveAs: () => void;
  readonly onUndo: () => void;
  readonly onRedo: () => void;
  readonly onValidate: () => void;
}

/**
 * Standard file handling for one workflow document: New, Open, Save, and Save
 * As through operating-system dialogs, followed by the document commands that
 * are not file operations. This surface deliberately has no Export and no Run
 * control: it writes files and commits to the workflow library, and it never
 * starts a Run or installs node implementations.
 */
export function WorkflowToolbar({
  workflowName,
  projectedVersion,
  editable,
  executable,
  validationCount,
  boundFilePath,
  draftSaved,
  canUndo,
  canRedo,
  saving,
  newDisabled,
  newTitle,
  openDisabled,
  openTitle,
  saveDisabled,
  saveTitle,
  saveAsDisabled,
  saveAsTitle,
  onNew,
  onOpen,
  onSave,
  onSaveAs,
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
              : "The editor holds changes the workflow library has not accepted yet"
          }
        >
          {draftSaved ? "✓ Draft saved" : "Unsaved changes"}
        </span>
        <span
          title={
            boundFilePath ??
            "No workflow file is bound yet: Save asks for a path first"
          }
        >
          {boundFilePath === null
            ? "No file"
            : `File · ${workflowFileNameFromPath(boundFilePath)}`}
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
          disabled={openDisabled}
          title={openTitle}
          type="button"
          onClick={onOpen}
        >
          Open
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
