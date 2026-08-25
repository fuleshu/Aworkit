import { useState } from "react";
import {
  bundledCreationDefaultTemplateId,
  bundledWorkflowTemplates,
} from "./bundledWorkflows";
import type {
  WorkflowLibrarySnapshot,
} from "./corePort";

interface WorkflowLibraryBarProps {
  readonly library: WorkflowLibrarySnapshot;
  readonly activeWorkflowId: string;
  readonly busy: boolean;
  readonly onSelect: (id: string) => void;
  readonly onCreate: (template: string, name: string) => void;
  readonly onDuplicate: (workflowId: string, name: string) => void;
  readonly onRename: (workflowId: string, name: string) => void;
  readonly onDelete: (workflowId: string) => void;
  readonly onSetDefault: (workflowId: string) => void;
}

const TEMPLATES = bundledWorkflowTemplates.map(({ templateId, name }) => ({
  value: templateId,
  label: name,
}));
const DEFAULT_TEMPLATE =
  bundledWorkflowTemplates.find(
    ({ templateId }) => templateId === bundledCreationDefaultTemplateId,
  )?.templateId ?? TEMPLATES[0]?.value ?? "";

/**
 * Compact saved-workflow library strip. All mutations cross the versioned
 * native `workflow_library` surface; the active selection is reloaded from the
 * workflow document snapshot by the parent editor.
 */
export function WorkflowLibraryBar({
  library,
  activeWorkflowId,
  busy,
  onSelect,
  onCreate,
  onDuplicate,
  onRename,
  onDelete,
  onSetDefault,
}: WorkflowLibraryBarProps): React.JSX.Element {
  const active = library.entries.find((entry) => entry.id === activeWorkflowId);
  const [template, setTemplate] = useState<string>(DEFAULT_TEMPLATE);
  const [name, setName] = useState("");
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
      <label>
        New from
        <select
          disabled={busy}
          title="Choose the template for a new workflow"
          value={template}
          onChange={(event) => setTemplate(event.target.value)}
        >
          {TEMPLATES.map(({ value, label }) => (
            <option key={value} value={value}>
              {label}
            </option>
          ))}
        </select>
      </label>
      <label>
        Name
        <input
          disabled={busy}
          placeholder="Workflow name"
          title="Name used by Create, Duplicate, and Rename"
          value={name}
          onChange={(event) => setName(event.target.value)}
        />
      </label>
      <button
        disabled={busy || name.trim() === ""}
        title="Create a new workflow from the selected template"
        type="button"
        onClick={() => onCreate(template, name)}
      >
        Create
      </button>
      <button
        disabled={busy || active === undefined}
        title="Duplicate the active workflow under the entered name"
        type="button"
        onClick={() => onDuplicate(activeWorkflowId, name || `${active?.name ?? "Workflow"} copy`)}
      >
        Duplicate
      </button>
      <button
        disabled={busy || active === undefined || name.trim() === ""}
        title="Rename the active workflow"
        type="button"
        onClick={() => onRename(activeWorkflowId, name)}
      >
        Rename
      </button>
      <button
        className="danger-action"
        disabled={busy || active === undefined || active.default || library.entries.length <= 1}
        title={
          active?.default
            ? "The default workflow cannot be deleted"
            : "Delete the active workflow"
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
