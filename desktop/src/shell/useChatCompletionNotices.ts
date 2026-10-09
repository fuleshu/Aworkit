import { useCallback, useEffect, useRef, useState } from "react";

/**
 * Tracks Chats that finish a Run while the user is not looking at them.
 *
 * A Chat leaves the projected `activeChatIds` when its command worker settles,
 * which is the one completion signal the shell already receives for background
 * Chats. Such a Chat keeps an unread marker in the history list until it is
 * opened; when the window itself is not focused the same completion also raises
 * an operating system notification through the caller's `notify`.
 */
export interface ChatCompletionTracking {
  /** Chats whose Run finished while they were not the visible Chat. */
  readonly unreadChatIds: ReadonlySet<string>;
  /** Clears one Chat's unread marker, for callers that know it was seen. */
  markRead: (chatId: string) => void;
}

export function useChatCompletionNotices(
  activeChatIds: readonly string[],
  selectedChatId: string | undefined,
  titleFor: (chatId: string) => string | undefined,
  notify: (title: string, body: string) => void,
): ChatCompletionTracking {
  const [unreadChatIds, setUnreadChatIds] = useState<ReadonlySet<string>>(
    () => new Set(),
  );
  const previousActive = useRef<ReadonlySet<string>>(new Set());
  const activeRef = useRef(activeChatIds);
  activeRef.current = activeChatIds;
  const selectedRef = useRef(selectedChatId);
  selectedRef.current = selectedChatId;
  const titleRef = useRef(titleFor);
  titleRef.current = titleFor;
  const notifyRef = useRef(notify);
  notifyRef.current = notify;
  // A new snapshot array with the same members must not re-run the transition
  // check on every 2s poll.
  const activeKey = [...activeChatIds].sort().join("\u0000");

  useEffect(() => {
    const active = new Set(activeRef.current);
    const finished = [...previousActive.current].filter(
      (chatId) => !active.has(chatId),
    );
    previousActive.current = active;
    if (finished.length === 0) return;
    setUnreadChatIds((current) => {
      let next: Set<string> | null = null;
      for (const chatId of finished) {
        if (chatId === selectedRef.current) continue;
        next ??= new Set(current);
        next.add(chatId);
      }
      return next ?? current;
    });
    // Only a window the user cannot see needs an operating system notification;
    // a focused window shows the blinking history marker instead.
    if (typeof document !== "undefined" && !document.hasFocus()) {
      for (const chatId of finished) {
        const title = titleRef.current(chatId) ?? "A Chat";
        notifyRef.current("Aworkit finished a reply", `${title} has a new reply.`);
      }
    }
    // `activeKey` is the stable signature of the members compared above.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeKey]);

  const markRead = useCallback((chatId: string) => {
    setUnreadChatIds((current) => {
      if (!current.has(chatId)) return current;
      const next = new Set(current);
      next.delete(chatId);
      return next;
    });
  }, []);

  // Opening a Chat is seeing it: its marker clears without an explicit call.
  useEffect(() => {
    if (selectedChatId === undefined) return;
    markRead(selectedChatId);
  }, [selectedChatId, markRead]);

  return { unreadChatIds, markRead };
}
