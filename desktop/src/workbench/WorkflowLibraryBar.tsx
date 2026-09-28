import type { WorkflowLibrarySnapshot } from "./corePort";

interface WorkflowLibraryBarProps {
  readonly library: WorkflowLibrarySnapshot;
  readonly activeWorkflowId: string;
  readonly busy: boolean;
  readonly onSelect: (id: string) => void;
  readonly onDelete: (workflowId: string) => void;
  readonly onSetDefault: (workflowId: string) => void;
}

/**
 * The saved-workflow strip: choose the workflow to edit, delete it, or make it
 * the profile default.
 *
 * The dropdown lists exactly the workflows the Aworkit workflow folder holds, so
 * every surface that names a workflow — this strip, the Chat workflow selector,
 * the editor title — is looking at the same list. Delete removes the workflow's
 * JSON document from that folder; the default workflow is never deleted.
 */
export function WorkflowLibraryBar({
  library,
  activeWorkflowId,
  busy,
  onSelect,
  onDelete,
  onSetDefault,
}: WorkflowLibraryBarProps): React.JSX.Element {
  const active = library.entries.find((entry) => entry.id === activeWorkflowId);
  return (
    <section className="workflow-library-bar" aria-label="Workflow library">
      <label>
        Workflow
        <select
          disabled={busy}
          title="Choose the saved workflow to open and edit"
          value={activeWorkflowId}
          onChange={(event) => onSelect(event.target.value)}
        >
          {library.entries.map((entry) => (
            <option key={entry.id} value={entry.id}>
              {entry.name}
              {entry.default ? " (default)" : ""}
            </option>
          ))}
        </select>
      </label>
      <button
        className="danger-action"
        disabled={busy || active === undefined || active.default}
        title={
          active?.default
            ? "The default workflow cannot be deleted"
            : "Delete the active workflow and its workflow JSON file"
        }
        type="button"
        onClick={() => onDelete(activeWorkflowId)}
      >
        Delete
      </button>
      <button
        disabled={busy || active === undefined || active.default}
        title="Make the active workflow the profile default"
        type="button"
        onClick={() => onSetDefault(activeWorkflowId)}
      >
        Set default
      </button>
    </section>
  );
}
