import { useState } from "react";
import {
  canSubmit,
  emptyComposer,
  submitIntent,
  updateComposer,
} from "./composer";
import type { ChatProjectChoice, ChatProjection } from "./types";

interface ChatComposerProps {
  readonly chat: ChatProjection;
  readonly projects: readonly ChatProjectChoice[];
  readonly stale: boolean;
  readonly pending: boolean;
  readonly workflowRequiresProject?: boolean | null;
  readonly workflowReadinessError?: string | null;
  readonly nextCommandId: () => string;
  readonly onSubmit: (
    intent: ReturnType<typeof submitIntent>,
  ) => Promise<boolean>;
}

/** Local IME-safe composer; only a committed core result is allowed to clear its draft. */
export function ChatComposer({
  chat,
  projects,
  stale,
  pending,
  workflowRequiresProject = false,
  workflowReadinessError = null,
  nextCommandId,
  onSubmit,
}: ChatComposerProps): React.JSX.Element {
  const [state, setState] = useState(emptyComposer);
  const [retryIntent, setRetryIntent] = useState<ReturnType<
    typeof submitIntent
  > | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const commandPending = pending || submitting;
  const edit = (patch: Parameters<typeof updateComposer>[1]) => {
    setRetryIntent(null);
    setState((current) => updateComposer(current, patch));
  };
  const disabledReason = stale
    ? "Reconnect and resynchronize before sending."
    : commandPending
      ? "The previous command is awaiting a committed core event."
      : canSubmit(state, chat, {
          workflowRequiresProject,
          workflowReadinessError,
        });
  const send = async () => {
    if (commandPending) return;
    setSubmitting(true);
    try {
      const intent =
        retryIntent ??
        submitIntent(state, chat, nextCommandId(), {
          workflowRequiresProject,
          workflowReadinessError,
        });
      setRetryIntent(intent);
      if (await onSubmit(intent)) {
        setRetryIntent(null);
        setState((current) =>
          updateComposer(current, { draft: "", attachments: [] }),
        );
      }
    } catch {
      /* The exact disabled reason remains visible next to the control. */
    } finally {
      setSubmitting(false);
    }
  };
  const selectedProjectId = chat.lockedWorkflow
    ? chat.projectId
    : state.projectId;
  const frozenProjectMissing =
    chat.lockedWorkflow &&
    chat.projectId !== null &&
    !projects.some(({ projectId }) => projectId === chat.projectId);
  const recoveryReason = chat.recoveryPending
    ? "Resume the interrupted command before composing another input."
    : null;
  return (
    <section className="composer-shell" aria-label="Chat composer">
      <div className="composer-meta">
        <label>
          Workflow
          <select
            aria-label="Workflow for the first Chat input"
            title="The first submitted input freezes this workflow for the Chat"
            value={state.workflowId}
            disabled={
              chat.lockedWorkflow || chat.recoveryPending || commandPending
            }
            onChange={(event) => edit({ workflowId: event.target.value })}
          >
            <option value="workflow.simple-chat">Simple Chat</option>
          </select>
        </label>
        <label>
          Project
          <select
            aria-label="Project for the first Chat input"
            title="The first submitted input resolves and freezes this saved project workspace; choose No project for an unscoped Chat"
            value={selectedProjectId ?? ""}
            disabled={
              chat.lockedWorkflow || chat.recoveryPending || commandPending
            }
            onChange={(event) =>
              edit({
                projectId: event.target.value === "" ? null : event.target.value,
              })
            }
          >
            <option value="">No project</option>
            {frozenProjectMissing && chat.projectId !== null && (
              <option value={chat.projectId}>{chat.scope}</option>
            )}
            {projects.map((project) => (
              <option key={project.projectId} value={project.projectId}>
                {project.name}
              </option>
            ))}
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
          aria-label="Add attachment references"
          disabled
          title="Attachments are unsupported in this build"
          type="button"
        >
          ＋
        </button>
        <textarea
          aria-label="Chat input"
          placeholder="Message Aworkit"
          disabled={chat.recoveryPending || commandPending}
          title={
            recoveryReason ??
            "Draft text stays local until the trusted core confirms a committed event"
          }
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
