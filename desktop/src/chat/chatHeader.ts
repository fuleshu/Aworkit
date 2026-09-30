import type { ChatProjection } from "./types";

/**
 * The browser preview fallback's placeholder Chat. It names no durable record,
 * so the header hides it rather than showing an id nothing can be looked up by.
 */
const PREVIEW_CHAT_ID = "chat.preview";

/**
 * The identity line under a Chat title.
 *
 * It writes the Chat's own `chat.`-prefixed id, the value the local store keys
 * `chat_streams` and `chat_projection` by, instead of the Run id. The prefix
 * states what the value names, so the same text can be handed to an agent and
 * resolved against storage without guessing whether it is a Chat or a Run.
 * Workflow and branch precede it, and empty fields drop out of the line.
 */
export function chatHeaderContext(chat: ChatProjection): string[] {
  return [
    chat.workflowName,
    chat.branch,
    chat.chatId === PREVIEW_CHAT_ID ? null : chat.chatId,
  ].filter((item): item is string => item !== null);
}
