import { useEffect, useRef, useState } from "react";

/**
 * The one text-input dialog behind New and Save As.
 *
 * A new or renamed workflow is named, never chosen by a path: the native
 * workflow library is the Aworkit workflow folder, and the name is what every
 * workflow dropdown shows. The dialog is a real modal — the native `<dialog>`
 * element owns the backdrop, the focus trap and Escape — and it keeps a refusal
 * (an empty name, a name another workflow already shows) next to the input
 * instead of closing and losing what the user typed.
 */
export function WorkflowNameDialog({
  title,
  inputLabel,
  confirmLabel,
  initialName,
  busy,
  error,
  onConfirm,
  onCancel,
}: {
  readonly title: string;
  readonly inputLabel: string;
  readonly confirmLabel: string;
  readonly initialName?: string;
  readonly busy: boolean;
  /** A refusal from the workflow library, shown without closing the dialog. */
  readonly error: string | null;
  readonly onConfirm: (name: string) => void;
  readonly onCancel: () => void;
}): React.JSX.Element {
  const dialog = useRef<HTMLDialogElement>(null);
  const [name, setName] = useState(initialName ?? "");
  useEffect(() => {
    const previous =
      document.activeElement instanceof HTMLElement ? document.activeElement : null;
    dialog.current?.showModal();
    return () => {
      if (previous?.isConnected) previous.focus({ preventScroll: true });
    };
  }, []);
  const trimmed = name.trim();
  return (
    <dialog
      ref={dialog}
      aria-labelledby="workflow-name-dialog-title"
      className="workbench-dialog workflow-name-dialog"
      onCancel={(event) => {
        event.preventDefault();
        if (!busy) onCancel();
      }}
    >
      <form
        onSubmit={(event) => {
          event.preventDefault();
          if (trimmed !== "" && !busy) onConfirm(trimmed);
        }}
      >
        <h2 id="workflow-name-dialog-title">{title}</h2>
        <label>
          {inputLabel}
          <input
            autoFocus
            disabled={busy}
            type="text"
            value={name}
            onChange={(event) => setName(event.target.value)}
          />
        </label>
        {error !== null && (
          <p className="workflow-name-error" role="alert">
            {error}
          </p>
        )}
        <div className="workflow-name-actions">
          <button
            disabled={busy}
            title="Close this dialog without changing any workflow"
            type="button"
            onClick={onCancel}
          >
            Cancel
          </button>
          <button
            className="primary-action"
            disabled={busy || trimmed === ""}
            title={
              trimmed === ""
                ? "Enter a workflow name first"
                : `${confirmLabel} as “${trimmed}”`
            }
            type="submit"
          >
            {busy ? "Working…" : confirmLabel}
          </button>
        </div>
      </form>
    </dialog>
  );
}
