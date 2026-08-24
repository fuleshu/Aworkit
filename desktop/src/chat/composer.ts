import type { ChatIntent, ChatProjection } from "./types";

export interface ComposerState {
  readonly draft: string;
  readonly attachments: readonly string[];
  readonly workflowId: string;
  readonly projectId: string | null;
  readonly imeComposing: boolean;
}

export interface ComposerReadiness {
  readonly workflowRequiresProject?: boolean | null;
  readonly workflowReadinessError?: string | null;
}

export const emptyComposer: ComposerState = {
  draft: "",
  attachments: [],
  workflowId: "workflow.simple-chat",
  projectId: null,
  imeComposing: false,
};

/** Local-only text and IME state. Drafts are cleared solely after an accepted receipt. */
export function updateComposer(
  state: ComposerState,
  patch: Partial<ComposerState>,
): ComposerState {
  return { ...state, ...patch };
}

export function canSubmit(
  state: ComposerState,
  chat: ChatProjection,
  readiness: ComposerReadiness = {},
): string | null {
  if (chat.recoveryPending)
    return "Resume the interrupted command before composing another input.";
  if (state.imeComposing) return "Finish IME composition before sending.";
  if (state.draft.trim() === "") return "Enter a message before sending.";
  if (chat.disabledReason !== undefined) return chat.disabledReason;
  if (["cancelled", "completed", "failed"].includes(chat.phase))
    return "This Chat is terminal. Start a new Chat to send another message.";
  if (!chat.lockedWorkflow) {
    if (readiness.workflowReadinessError !== null && readiness.workflowReadinessError !== undefined)
      return readiness.workflowReadinessError;
    if (readiness.workflowRequiresProject === null)
      return "Checking the saved Simple Chat workflow before sending.";
    if (
      readiness.workflowRequiresProject === true &&
      state.projectId === null
    )
      return "Select a saved project before sending because Simple Chat binds project file read/search.";
  }
  return null;
}

export function submitIntent(
  state: ComposerState,
  chat: ChatProjection,
  commandId: string,
  readiness: ComposerReadiness = {},
): ChatIntent {
  const reason = canSubmit(state, chat, readiness);
  if (reason !== null) throw new Error(reason);
  return chat.lockedWorkflow
    ? { type: "enqueue", commandId, input: state.draft }
    : {
        type: "start",
        commandId,
        workflowId: state.workflowId,
        projectId: state.projectId,
        input: state.draft,
        attachments: [],
      };
}

export function controlsFor(
  chat: ChatProjection,
): readonly ChatIntent["type"][] {
  if (
    chat.phase === "running" ||
    chat.phase === "paused" ||
    chat.phase === "awaiting_approval"
  )
    return ["cancel"];
  return [];
}
