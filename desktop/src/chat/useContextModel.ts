import { useEffect, useState } from "react";
import type { ChatCorePort } from "./corePort";
import type { ContextModel } from "./contextProjection";

/** Resolve missing capacity once per visible Chat/workflow and discard stale responses. */
export function useContextModel(read: ChatCorePort["contextModel"], chatId: string | undefined,
  workflowId: string | null, fallback: ContextModel | null | undefined) {
  const [resolved, setResolved] = useState<{ key: string; model: ContextModel | null } | null>(null);
  const key = JSON.stringify([chatId, workflowId]);
  useEffect(() => {
    if (!read || !chatId || fallback?.contextWindow) return;
    let current = true;
    void read(chatId, workflowId).then(model => { if (current) setResolved({ key, model }); }).catch(() => {});
    return () => { current = false; };
  }, [read, chatId, workflowId, key, fallback?.contextWindow]);
  return fallback?.contextWindow ? fallback : resolved?.key === key ? resolved.model ?? fallback : fallback;
}
