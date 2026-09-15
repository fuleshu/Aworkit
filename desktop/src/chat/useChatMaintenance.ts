import { useCallback, useRef, useState } from "react";
import type { ChatIntent } from "./types";

/** Maintenance and its FIFO belong to a Chat even after navigation. */
export function useChatMaintenance(execute: (intent: ChatIntent, version?: number) => Promise<boolean>) {
  const active = useRef(new Set<string>());
  const queues = useRef(new Map<string, ChatIntent[]>());
  const draining = useRef(new Set<string>());
  const [, changed] = useState(0);
  const publish = () => changed(value => value + 1);
  const drain = useCallback(async (chatId: string) => {
    if (active.current.has(chatId) || draining.current.has(chatId)) return;
    draining.current.add(chatId); publish();
    try {
      const queue = queues.current.get(chatId);
      while (queue?.length) {
        if (!await execute(queue[0])) break;
        queue.shift(); publish();
      }
      if (!queue?.length) queues.current.delete(chatId);
    } finally { draining.current.delete(chatId); publish(); }
  }, [execute]);
  const dispatch = useCallback(async (intent: ChatIntent, version?: number): Promise<boolean> => {
    const chatId = intent.targetId ?? "";
    if (intent.type === "enqueue" && (active.current.has(chatId) || draining.current.has(chatId) || queues.current.has(chatId))) {
      const queue = queues.current.get(chatId) ?? [];
      if (!queue.some(entry => entry.commandId === intent.commandId)) queue.push(intent);
      queues.current.set(chatId, queue);
      publish();
      // An explicit new input can retry a failed queue without bypassing its
      // oldest entry. Never start an automatic retry loop after a rejection.
      await drain(chatId);
      return true;
    }
    if (intent.type !== "compact_context") return execute(intent, version);
    active.current.add(chatId); publish();
    try { return await execute(intent, version); }
    finally {
      active.current.delete(chatId);
      await drain(chatId);
    }
  }, [drain, execute]);
  return {
    dispatch,
    pending: (chatId: string) => active.current.has(chatId),
    inputs: (chatId: string) => (queues.current.get(chatId) ?? []).flatMap(entry => entry.type === "enqueue" ? [entry.input] : []),
  };
}
