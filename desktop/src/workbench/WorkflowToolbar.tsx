import { useRef } from "react";

interface WorkflowToolbarProps {
  readonly workflowName: string;
  readonly projectedVersion: number;
  readonly editable: boolean;
  readonly executable: boolean;
  readonly validationCount: number;
  readonly canUndo: boolean;
  readonly canRedo: boolean;
  readonly saving: boolean;
  readonly saveDisabled: boolean;
  readonly saveTitle: string;
  readonly runDisabled: boolean;
  readonly runTitle: string;
  readonly onImport: (file: File) => void;
  readonly onUndo: () => void;
  readonly onRedo: () => void;
  readonly onValidate: () => void;
  readonly onSave: () => void;
  readonly onExport: () => void;
  readonly onRun?: () => void;
}

export function WorkflowToolbar({
  workflowName,
  projectedVersion,
  editable,
  executable,
  validationCount,
  canUndo,
  canRedo,
  saving,
  saveDisabled,
  saveTitle,
  runDisabled,
  runTitle,
  onImport,
  onUndo,
  onRedo,
  onValidate,
  onSave,
  onExport,
  onRun,
}: WorkflowToolbarProps): React.JSX.Element {
  const importInput = useRef<HTMLInputElement>(null);
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
        <input
          accept="application/json,.json"
          aria-label="Workflow JSON file"
          hidden
          ref={importInput}
          title="Choose a workflow JSON document to import locally"
          type="file"
          onChange={(event) => {
            const file = event.target.files?.[0];
            if (file !== undefined) onImport(file);
            event.target.value = "";
          }}
        />
        <button
          title="Import a workflow JSON document without installing or enabling code"
          type="button"
          onClick={() => importInput.current?.click()}
        >
          Import JSON
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
          title="Export the complete lossless workflow JSON document"
          type="button"
          onClick={onExport}
        >
          Export
        </button>
        <button
          disabled={runDisabled}
          title={runTitle}
          type="button"
          onClick={onRun}
        >
          Run
        </button>
      </div>
    </header>
  );
}
