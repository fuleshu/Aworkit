import { useEffect, useState } from "react";
import { ImageAttachmentMenu, ImageAttachments } from "./ImageAttachments";
import { useChatImages } from "./useChatImages";
import {
  canSubmit,
  emptyComposer,
  submitIntent,
  updateComposer,
  type ComposerState,
} from "./composer";
import type { ChatProjectChoice, ChatProjection } from "./types";
import {
  bundledDefaultWorkflowId,
  bundledWorkflowTemplates,
} from "../workbench/bundledWorkflows";

export interface WorkflowOption {
  readonly id: string;
  readonly name: string;
}

interface ChatComposerProps {
  readonly chat: ChatProjection;
  readonly projects: readonly ChatProjectChoice[];
  readonly stale: boolean;
  readonly pending: boolean;
  readonly workflows?: readonly WorkflowOption[];
  readonly defaultWorkflowId?: string | null;
  readonly workflowRequiresProject?: boolean | null;
  readonly workflowReadinessError?: string | null;
  readonly nextCommandId: () => string;
  readonly onWorkflowChange?: (workflowId: string) => void;
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
  workflows,
  defaultWorkflowId,
  workflowRequiresProject = false,
  workflowReadinessError = null,
  nextCommandId,
  onWorkflowChange,
  onSubmit,
}: ChatComposerProps): React.JSX.Element {
  const workflowOptions: readonly WorkflowOption[] =
    workflows ??
    bundledWorkflowTemplates
      .filter(({ seedOnFreshProfile }) => seedOnFreshProfile)
      .map(({ workflowId, name }) => ({ id: workflowId, name }));
  const visibleWorkflowOptions: readonly WorkflowOption[] = chat.lockedWorkflow
    ? chat.workflowId === null
      ? [{ id: "", name: chat.workflowName ?? "Unavailable workflow" }]
      : workflowOptions.some(({ id }) => id === chat.workflowId)
        ? workflowOptions
        : [
            ...workflowOptions,
            { id: chat.workflowId, name: chat.workflowName ?? chat.workflowId },
          ]
    : workflowOptions;
  const [state, setState] = useState<ComposerState>(() => ({
    ...emptyComposer,
    workflowId: chat.lockedWorkflow
      ? (chat.workflowId ?? "")
      : (defaultWorkflowId ??
        (workflows === undefined
          ? bundledDefaultWorkflowId
          : workflowOptions[0]?.id) ??
        ""),
  }));
  useEffect(() => {
    onWorkflowChange?.(state.workflowId);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const [retryIntent, setRetryIntent] = useState<ReturnType<
    typeof submitIntent
  > | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const edit = (patch: Parameters<typeof updateComposer>[1]) => {
    setRetryIntent(null);
    setState((current) => updateComposer(current, patch));
  };
  const {
    addFiles,
    importing,
    error: imageError,
  } = useChatImages(state.attachments, (attachments) => edit({ attachments }));
  const commandPending = pending || submitting || importing;
  const disabledReason = stale
    ? "Reconnect and resynchronize before sending."
    : importing
      ? "Adding images…"
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
    <section
      className="composer-shell"
      aria-label="Chat composer"
      onPaste={(event) => {
        if (chat.recoveryPending || commandPending) return;
        const files = Array.from(event.clipboardData.files);
        if (files.length === 0) return;
        event.preventDefault();
        const text = event.clipboardData.getData("text/plain");
        if (text) edit({ draft: state.draft + text });
        void addFiles(files);
      }}
    >
      <div className="composer-meta">
        <label>
          Workflow
          <select
            aria-label="Workflow for the first Chat input"
            title="The first submitted input freezes this workflow for the Chat"
            value={state.workflowId}
            disabled={
              chat.lockedWorkflow ||
              chat.recoveryPending ||
              commandPending ||
              visibleWorkflowOptions.length === 0
            }
            onChange={(event) => {
              edit({ workflowId: event.target.value });
              onWorkflowChange?.(event.target.value);
            }}
          >
            {visibleWorkflowOptions.length === 0 && (
              <option value="">No workflows available</option>
            )}
            {visibleWorkflowOptions.map((workflow) => (
              <option key={workflow.id} value={workflow.id}>
                {workflow.name}
              </option>
            ))}
            {state.workflowId !== "" &&
              !visibleWorkflowOptions.some(
                (workflow) => workflow.id === state.workflowId,
              ) && <option value={state.workflowId}>{state.workflowId}</option>}
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
                projectId:
                  event.target.value === "" ? null : event.target.value,
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
      <ImageAttachments
        images={state.attachments}
        disabled={chat.recoveryPending || commandPending}
        onRemove={(index) =>
          edit({
            attachments: state.attachments.filter(
              (_, candidate) => candidate !== index,
            ),
          })
        }
      />
      {imageError !== null && (
        <p className="chat-image-error" role="alert">
          {imageError}
        </p>
      )}
      <div className="composer-input">
        <ImageAttachmentMenu
          disabled={chat.recoveryPending || commandPending}
          onFiles={(files) => void addFiles(files)}
        />
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
              ? "Enter to send · Shift+Enter for a new line · Paste to add images"
              : "Retry will reuse the same idempotent command ID")}
        </span>
        <span>{state.draft.length} characters</span>
      </div>
    </section>
  );
}
