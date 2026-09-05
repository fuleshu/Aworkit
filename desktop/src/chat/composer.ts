import type { ChatIntent, ChatProjection } from "./types";
import type { ImageAttachment } from "./images";
import { bundledDefaultWorkflowId } from "../workbench/bundledWorkflows";

export interface ComposerState {
  readonly draft: string;
  readonly attachments: readonly ImageAttachment[];
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
  workflowId: bundledDefaultWorkflowId,
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
  if (state.draft.trim() === "" && state.attachments.length === 0)
    return "Enter a message or add an image before sending.";
  if (chat.disabledReason !== undefined) return chat.disabledReason;
  if (["cancelled", "completed", "failed"].includes(chat.phase))
    return "This Chat is terminal. Start a new Chat to send another message.";
  if (!chat.lockedWorkflow) {
    if (state.workflowId === "")
      return "Select a saved workflow before sending.";
    if (
      readiness.workflowReadinessError !== null &&
      readiness.workflowReadinessError !== undefined
    )
      return readiness.workflowReadinessError;
    if (readiness.workflowRequiresProject === null)
      return "Checking the saved workflow before sending.";
    if (readiness.workflowRequiresProject === true && state.projectId === null)
      return "Select a saved project before sending because the selected workflow binds project file tools.";
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
    ? {
        type: "enqueue",
        commandId,
        input: state.draft,
        ...(state.attachments.length > 0
          ? { attachments: state.attachments }
          : {}),
      }
    : {
        type: "start",
        commandId,
        workflowId: state.workflowId,
        projectId: state.projectId,
        input: state.draft,
        attachments: state.attachments,
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
