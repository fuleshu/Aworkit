import { useState } from "react";
import {
  canSubmit,
  emptyComposer,
  submitIntent,
  updateComposer,
} from "./composer";
import type { ChatProjection } from "./types";

interface ChatComposerProps {
  readonly chat: ChatProjection;
  readonly stale: boolean;
  readonly pending: boolean;
  readonly nextCommandId: () => string;
  readonly onSubmit: (
    intent: ReturnType<typeof submitIntent>,
  ) => Promise<boolean>;
}

/** Local IME-safe composer; only a committed core result is allowed to clear its draft. */
export function ChatComposer({
  chat,
  stale,
  pending,
  nextCommandId,
  onSubmit,
}: ChatComposerProps): React.JSX.Element {
  const [state, setState] = useState(emptyComposer);
  const [retryIntent, setRetryIntent] = useState<ReturnType<
    typeof submitIntent
  > | null>(null);
  const [attachmentsOpen, setAttachmentsOpen] = useState(false);
  const edit = (patch: Parameters<typeof updateComposer>[1]) => {
    setRetryIntent(null);
    setState((current) => updateComposer(current, patch));
  };
  const disabledReason = stale
    ? "Reconnect and resynchronize before sending."
    : pending
      ? "The previous command is awaiting a committed core event."
      : canSubmit(state, chat);
  const send = async () => {
    try {
      const intent = retryIntent ?? submitIntent(state, chat, nextCommandId());
      setRetryIntent(intent);
      if (await onSubmit(intent)) {
        setRetryIntent(null);
        setState((current) =>
          updateComposer(current, { draft: "", attachments: [] }),
        );
      }
    } catch {
      /* The exact disabled reason remains visible next to the control. */
    }
  };
  return (
    <section className="composer-shell" aria-label="Chat composer">
      <div className="composer-meta">
        <label>
          Workflow
          <select
            aria-label="Workflow for the first Chat input"
            title="The first submitted input freezes this workflow for the Chat"
            value={state.workflowId}
            disabled={chat.lockedWorkflow}
            onChange={(event) => edit({ workflowId: event.target.value })}
          >
            <option value="workflow.repository-engineer">
              Repository Engineer
            </option>
            <option value="workflow.review">Review workflow</option>
          </select>
        </label>
        {chat.lockedWorkflow && (
          <span
            className="workflow-lock"
            title="The workflow is immutable after the first input"
          >
            ▣ Workflow locked
          </span>
        )}
        <span className="queue-count">{chat.queuedInputs.length} queued</span>
      </div>
      <div className="composer-input">
        <button
          aria-expanded={attachmentsOpen}
          aria-label="Add attachment references"
          disabled={chat.lockedWorkflow}
          title={
            chat.lockedWorkflow
              ? "Attachment references are frozen with the first Chat input"
              : "Add attachment or context references"
          }
          type="button"
          onClick={() => setAttachmentsOpen((open) => !open)}
        >
          ＋
        </button>
        <textarea
          aria-label="Chat input"
          placeholder="Message Aworkit"
          title="Draft text stays local until the trusted core confirms a committed event"
          value={state.draft}
          onCompositionStart={() => edit({ imeComposing: true })}
          onCompositionEnd={() => edit({ imeComposing: false })}
          onKeyDown={(event) => {
            if (
              event.key === "Enter" &&
              !event.shiftKey &&
              !state.imeComposing
            ) {
              event.preventDefault();
              void send();
            }
          }}
          onChange={(event) => edit({ draft: event.target.value })}
        />
        <button
          className="primary-action"
          disabled={disabledReason !== null}
          title={
            disabledReason ??
            (chat.lockedWorkflow ? "Queue this input" : "Start this Chat")
          }
          type="button"
          onClick={() => void send()}
        >
          {chat.lockedWorkflow ? "Queue" : "Send"}
        </button>
      </div>
      {chat.queuedInputs.length > 0 && (
        <details className="queued-input-preview">
          <summary>{chat.queuedInputs.length} queued input(s)</summary>
          <ol>
            {chat.queuedInputs.map((input, index) => (
              <li key={`${index}-${input}`}>{input}</li>
            ))}
          </ol>
        </details>
      )}
      {attachmentsOpen && (
        <label className="attachment-entry">
          Attachment references
          <input
            aria-label="Attachment references"
            title="Comma-separated local references; the trusted core validates every selected path"
            value={state.attachments.join(", ")}
            onChange={(event) =>
              edit({
                attachments: event.target.value
                  .split(",")
                  .map((value) => value.trim())
                  .filter(Boolean),
              })
            }
          />
        </label>
      )}
      <div className="composer-footer">
        <span>
          {disabledReason ??
            (retryIntent === null
              ? "Enter to send · Shift+Enter for a new line"
              : "Retry will reuse the same idempotent command ID")}
        </span>
        <span>{state.draft.length} characters</span>
      </div>
    </section>
  );
}
