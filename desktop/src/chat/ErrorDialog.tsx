import { useEffect, useId, useRef } from "react";

export interface ErrorDialogNotice {
  readonly key: string;
  readonly title: string;
  readonly body: string;
}

/** Acknowledged modal used for errors that require the user's attention. */
export function ErrorDialog({
  notice,
  onDismiss,
}: {
  readonly notice: ErrorDialogNotice;
  readonly onDismiss: () => void;
}): React.JSX.Element {
  const titleId = useId();
  const bodyId = useId();
  const dialogRef = useRef<HTMLElement>(null);
  const okRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    const previous =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;
    okRef.current?.focus();
    return () => previous?.focus();
  }, [notice.key]);

  return (
    <div className="dialog-backdrop">
      <section
        aria-describedby={bodyId}
        aria-labelledby={titleId}
        aria-modal="true"
        className="workbench-dialog error-dialog"
        ref={dialogRef}
        role="alertdialog"
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            onDismiss();
            return;
          }
          if (event.key !== "Tab") return;
          const controls = Array.from(
            dialogRef.current?.querySelectorAll<HTMLElement>(
              'button:not(:disabled), [href], input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex]:not([tabindex="-1"])',
            ) ?? [],
          );
          const first = controls[0];
          const last = controls.at(-1);
          if (first === undefined || last === undefined) return;
          if (event.shiftKey && document.activeElement === first) {
            event.preventDefault();
            last.focus();
          } else if (!event.shiftKey && document.activeElement === last) {
            event.preventDefault();
            first.focus();
          }
        }}
      >
        <h2 id={titleId}>{notice.title}</h2>
        <p id={bodyId}>{notice.body}</p>
        <div>
          <button
            className="primary-action"
            ref={okRef}
            title="Acknowledge this error"
            type="button"
            onClick={onDismiss}
          >
            OK
          </button>
        </div>
      </section>
    </div>
  );
}
