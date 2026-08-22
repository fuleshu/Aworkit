import type { ChatIntent, ChatProjection } from "./types";

export interface ComposerState {
  readonly draft: string;
  readonly attachments: readonly string[];
  readonly workflowId: string;
  readonly imeComposing: boolean;
}

export const emptyComposer: ComposerState = {
  draft: "",
  attachments: [],
  workflowId: "workflow.repository-engineer",
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
): string | null {
  if (state.imeComposing) return "Finish IME composition before sending.";
  if (state.draft.trim() === "") return "Enter a message before sending.";
  if (chat.disabledReason !== undefined) return chat.disabledReason;
  if (["cancelled", "completed", "failed"].includes(chat.phase))
    return "This Chat is terminal; fork or continue in a new Chat.";
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
    ? { type: "enqueue", commandId, input: state.draft }
    : {
        type: "start",
        commandId,
        workflowId: state.workflowId,
        input: state.draft,
        attachments: state.attachments,
      };
}

export function controlsFor(
  chat: ChatProjection,
): readonly ChatIntent["type"][] {
  if (chat.phase === "running") return ["pause", "cancel"];
  if (chat.phase === "paused") return ["resume", "cancel"];
  if (chat.phase === "awaiting_approval") return ["cancel"];
  if (chat.phase === "failed") return ["retry", "fork", "continue"];
  if (chat.phase === "cancelled" || chat.phase === "completed")
    return ["fork", "continue"];
  return [];
}
