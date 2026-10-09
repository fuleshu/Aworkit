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

/** The one disabled reason that needs no explanation because the draft is empty. */
export const emptyDraftReason = "Enter a message or add an image before sending.";

export const emptyComposer: ComposerState = {
  draft: "",
  attachments: [],
  workflowId: bundledDefaultWorkflowId,
  projectId: null,
  imeComposing: false,
};

/** Local-only text and IME state. Clear drafts after committed input or an accepted receipt. */
export function updateComposer(
  state: ComposerState,
  patch: Partial<ComposerState>,
): ComposerState {
  return { ...state, ...patch };
}

/**
 * The reason Send cannot run, or null.
 *
 * Workflow/model readiness is deliberately not a disable condition: a workflow
 * that cannot start is reported by the core refusal and by the composer's own
 * readiness notice, never by a dead button the user cannot explain.
 */
export function canSubmit(
  state: ComposerState,
  chat: ChatProjection,
): string | null {
  if (chat.recoveryPending)
    return "Continue or stop the interrupted reply to send a new message.";
  if (state.imeComposing) return "Finish IME composition before sending.";
  if (state.draft.trim() === "" && state.attachments.length === 0)
    return emptyDraftReason;
  if (chat.disabledReason !== undefined) return chat.disabledReason;
  if (chat.phase === "awaiting_answer")
    return "Answer or skip the agent's question to continue this Run.";
  if (["cancelled", "completed", "failed"].includes(chat.phase))
    return "This Chat is terminal. Start a new Chat to send another message.";
  if (!chat.lockedWorkflow && state.workflowId === "")
    return "Select a saved workflow before sending.";
  return null;
}

export function submitIntent(
  state: ComposerState,
  chat: ChatProjection,
  commandId: string,
): ChatIntent {
  const reason = canSubmit(state, chat);
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
  // `cancelling` keeps the Stop control available: a turn that is slow to settle
  // must remain stoppable until it actually leaves the live phase.
  if (
    chat.phase === "running" ||
    chat.phase === "paused" ||
    chat.phase === "awaiting_approval" ||
    chat.phase === "awaiting_answer" ||
    chat.phase === "cancelling"
  )
    return ["cancel"];
  return [];
}
