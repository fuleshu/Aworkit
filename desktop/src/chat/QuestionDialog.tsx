import { useEffect, useId, useRef, useState } from "react";
import { MarkdownContent } from "./MarkdownContent";
import {
  initialQuestionDraft,
  questionAnswerInput,
  questionDraftComplete,
  questionKindLabel,
  type QuestionAnswerInput,
  type QuestionDraft,
  type QuestionModel,
} from "./question";
import "./question.css";

interface QuestionDialogProps {
  readonly question: QuestionModel;
  readonly busy: boolean;
  readonly error: string | null;
  /** Opens the operating system's own file or folder chooser. */
  readonly pickPath?: (
    kind: "file" | "folder",
    extensions: readonly string[],
  ) => Promise<string | null>;
  readonly onAnswer: (answer: QuestionAnswerInput) => void;
  /** Closes the dialog without answering; the question stays on its card. */
  readonly onDismiss: () => void;
}

/**
 * The focused answering surface for one model question.
 *
 * It is a real modal: the native `<dialog>` element owns the backdrop, the
 * focus trap, Escape and focus restoration. Dismissing it never answers the
 * question — the durable card stays answerable — and a question that is already
 * answered or cancelled is never mounted again by the workspace.
 */
export function QuestionDialog({
  question,
  busy,
  error,
  pickPath,
  onAnswer,
  onDismiss,
}: QuestionDialogProps): React.JSX.Element {
  const dialog = useRef<HTMLDialogElement>(null);
  const [draft, setDraft] = useState<QuestionDraft>(() =>
    initialQuestionDraft(question),
  );
  const [picking, setPicking] = useState(false);
  const headingId = useId();
  const complete = questionDraftComplete(question, draft);

  useEffect(() => {
    const previous =
      document.activeElement instanceof HTMLElement ? document.activeElement : null;
    dialog.current?.showModal();
    return () => {
      if (previous?.isConnected) previous.focus({ preventScroll: true });
    };
  }, []);

  const choose = async () => {
    if (pickPath === undefined || busy || picking) return;
    setPicking(true);
    try {
      const path = await pickPath(
        question.kind === "folder" ? "folder" : "file",
        question.extensions,
      );
      // A dismissed operating-system dialog leaves this dialog unchanged.
      if (path !== null) setDraft((current) => ({ ...current, path }));
    } finally {
      setPicking(false);
    }
  };

  return (
    <dialog
      ref={dialog}
      className="workbench-dialog question-dialog"
      aria-labelledby={headingId}
      onCancel={(event) => {
        event.preventDefault();
        if (!busy) onDismiss();
      }}
    >
      <header className="question-dialog-heading">
        <p className="eyebrow">QUESTION</p>
        <h2 id={headingId}>{question.title ?? questionKindLabel(question.kind)}</h2>
      </header>
      <MarkdownContent className="question-dialog-prompt">
        {question.prompt}
      </MarkdownContent>
      {question.kind === "choice" ? (
        <fieldset className="question-options" disabled={busy}>
          <legend>Your answer</legend>
          {question.options.map((option, index) => (
            <label key={option.id}>
              <input
                autoFocus={index === 0}
                checked={draft.optionId === option.id}
                name={`question-${question.questionId}`}
                title={`Answer with ${option.label}`}
                type="radio"
                value={option.id}
                onChange={() =>
                  setDraft((current) => ({ ...current, optionId: option.id }))
                }
              />
              <span>
                <strong>{option.label}</strong>
                {option.description !== undefined && (
                  <small>{option.description}</small>
                )}
              </span>
            </label>
          ))}
        </fieldset>
      ) : null}
      {question.kind === "choice" && question.allowFreeText ? (
        <label className="question-free-text">
          Your own answer
          <textarea
            aria-label="Your own answer"
            disabled={busy}
            rows={3}
            title="Answer in your own words instead of choosing an option"
            value={draft.freeText}
            onChange={(event) =>
              setDraft((current) => ({ ...current, freeText: event.target.value }))
            }
          />
        </label>
      ) : null}
      {question.kind !== "choice" ? (
        <div className="question-path">
          <p>
            {draft.path ?? `No ${question.kind} chosen yet.`}
          </p>
          <button
            type="button"
            disabled={busy || picking || pickPath === undefined}
            title={
              pickPath === undefined
                ? "This desktop cannot open the operating system's chooser"
                : `Open the operating system's ${question.kind} chooser`
            }
            onClick={() => void choose()}
          >
            {picking
              ? "Choosing…"
              : question.kind === "folder"
                ? "Choose folder…"
                : "Choose file…"}
          </button>
        </div>
      ) : null}
      {error !== null && (
        <p className="question-dialog-error" role="alert">
          {error}
        </p>
      )}
      <div className="question-dialog-actions">
        <button
          type="button"
          className="primary-action"
          disabled={busy || !complete}
          title={
            complete
              ? "Send this answer to the agent and continue the Run"
              : "Choose an option or write an answer first"
          }
          onClick={() => onAnswer(questionAnswerInput(question, draft))}
        >
          {busy ? "Sending…" : "Submit answer"}
        </button>
        <button
          type="button"
          disabled={busy}
          title="Skip this question without answering it; the agent continues without that answer"
          onClick={() => onAnswer({ cancelled: true })}
        >
          Skip
        </button>
        <button
          type="button"
          disabled={busy}
          title="Close this dialog and answer from the question card later"
          onClick={onDismiss}
        >
          Decide later
        </button>
      </div>
    </dialog>
  );
}
