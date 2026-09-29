import { useEffect, useId, useRef, useState } from "react";
import { folderLeaf, type NewProjectKind, type ProjectDraft } from "./createProject";
import type { ChatProjectChoice } from "./types";
import "./createProject.css";

const workspaceKinds: readonly {
  readonly value: NewProjectKind;
  readonly label: string;
}[] = [
  { value: "local_directory", label: "Local directory" },
  { value: "git_worktree", label: "Git worktree" },
  { value: "container_mount", label: "Container mount" },
];

interface CreateProjectDialogProps {
  /** Opens the operating system's own folder chooser. */
  readonly pickFolder?: () => Promise<string | null>;
  /**
   * Saves the project. It rejects when the record was not persisted, and the
   * dialog keeps the entered values so the user can retry.
   */
  readonly create: (draft: ProjectDraft) => Promise<ChatProjectChoice>;
  readonly onCreated: (project: ChatProjectChoice) => void;
  /** Closes without creating anything. */
  readonly onCancel: () => void;
}

/**
 * The Chat's "Create New Project…" dialog.
 *
 * It collects exactly what Settings → Projects stores for a project (name,
 * workspace kind, workspace location), so a project created here is
 * indistinguishable from one created in Settings. It is a real modal: the
 * native `<dialog>` element owns the backdrop, focus trap, Escape and focus
 * restoration, and cancelling never writes anything.
 */
export function CreateProjectDialog({
  pickFolder,
  create,
  onCreated,
  onCancel,
}: CreateProjectDialogProps): React.JSX.Element {
  const dialog = useRef<HTMLDialogElement>(null);
  const headingId = useId();
  const [name, setName] = useState("");
  const [kind, setKind] = useState<NewProjectKind>("local_directory");
  const [location, setLocation] = useState("");
  // A name the user typed is never overwritten by a later folder choice.
  const [nameEdited, setNameEdited] = useState(false);
  const [picking, setPicking] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const complete = name.trim() !== "" && location.trim() !== "";

  useEffect(() => {
    const previous =
      document.activeElement instanceof HTMLElement ? document.activeElement : null;
    dialog.current?.showModal();
    return () => {
      if (previous?.isConnected) previous.focus({ preventScroll: true });
    };
  }, []);

  const browse = async () => {
    if (pickFolder === undefined || picking || saving) return;
    setPicking(true);
    setError(null);
    try {
      const chosen = await pickFolder();
      // A dismissed operating-system dialog leaves this dialog unchanged.
      if (chosen === null) return;
      setLocation(chosen);
      if (!nameEdited) setName(folderLeaf(chosen));
    } finally {
      setPicking(false);
    }
  };

  const submit = async () => {
    if (!complete || saving) return;
    setSaving(true);
    setError(null);
    try {
      // Success unmounts this dialog; nothing after it may set state.
      onCreated(
        await create({ name: name.trim(), kind, location: location.trim() }),
      );
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
      setSaving(false);
    }
  };

  return (
    <dialog
      ref={dialog}
      className="workbench-dialog create-project-dialog"
      aria-labelledby={headingId}
      onCancel={(event) => {
        event.preventDefault();
        if (!saving) onCancel();
      }}
    >
      <form
        onSubmit={(event) => {
          event.preventDefault();
          void submit();
        }}
      >
        <header className="create-project-heading">
          <p className="eyebrow">PROJECT</p>
          <h2 id={headingId}>Create project</h2>
        </header>
        <p className="create-project-intro">
          A project binds a stable Aworkit identity to one explicit workspace.
          It is saved to Settings and selected in this Chat once created.
        </p>
        <label className="settings-field" htmlFor="create-project-name">
          Project name
          <input
            autoFocus
            id="create-project-name"
            disabled={saving}
            maxLength={256}
            placeholder="Project"
            title="Name shown in Aworkit navigation and Chat scope"
            type="text"
            value={name}
            onChange={(event) => {
              setNameEdited(true);
              setName(event.target.value);
            }}
          />
        </label>
        <label className="settings-field" htmlFor="create-project-kind">
          Workspace kind
          <select
            id="create-project-kind"
            disabled={saving}
            title="How this prepared workspace is provided to Aworkit"
            value={kind}
            onChange={(event) => setKind(event.target.value as NewProjectKind)}
          >
            {workspaceKinds.map((option) => (
              <option key={option.value} value={option.value}>
                {option.label}
              </option>
            ))}
          </select>
        </label>
        <label className="settings-field" htmlFor="create-project-location">
          Workspace folder
          <input
            id="create-project-location"
            disabled={saving}
            maxLength={4096}
            placeholder="Choose a folder…"
            spellCheck={false}
            title="The prepared workspace this project binds to"
            type="text"
            value={location}
            onChange={(event) => setLocation(event.target.value)}
          />
        </label>
        <div className="create-project-browse">
          <button
            type="button"
            disabled={saving || picking || pickFolder === undefined}
            title={
              pickFolder === undefined
                ? "This desktop cannot open the operating system's folder chooser"
                : "Open the operating system's folder chooser"
            }
            onClick={() => void browse()}
          >
            {picking ? "Choosing…" : "Browse…"}
          </button>
        </div>
        {error !== null && (
          <p className="create-project-error" role="alert">
            {error}
          </p>
        )}
        <div className="create-project-actions">
          <button
            className="primary-action"
            type="submit"
            disabled={saving || !complete}
            title={
              complete
                ? "Save this project to Settings and select it in this Chat"
                : "Name the project and choose its workspace folder first"
            }
          >
            {saving ? "Creating…" : "Create project"}
          </button>
          <button
            type="button"
            disabled={saving}
            title="Close without creating a project"
            onClick={onCancel}
          >
            Cancel
          </button>
        </div>
      </form>
    </dialog>
  );
}
