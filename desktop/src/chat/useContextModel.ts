import { useEffect, useState } from "react";
import type { ChatCorePort } from "./corePort";
import type { ContextModel } from "./contextProjection";

/**
 * Resolve missing capacity once per visible Chat/workflow and discard stale responses.
 *
 * The trusted core's projection names the exact model a frozen Chat will use, so
 * that name always wins: a tier resolved before a Settings change must never
 * outlive it and make the surface report the previous provider/model. A draft
 * Chat has no authoritative name yet, so it re-resolves whenever the surface
 * becomes active again — returning from Settings refreshes the tier that the next
 * Run will resolve.
 */
export function useContextModel(read: ChatCorePort["contextModel"], chatId: string | undefined,
  workflowId: string | null, fallback: ContextModel | null | undefined, active = true) {
  const [resolved, setResolved] = useState<{ key: string; model: ContextModel | null } | null>(null);
  const key = JSON.stringify([chatId, workflowId, fallback?.name ?? null, active]);
  useEffect(() => {
    if (!read || !chatId || !active) return;
    if (fallback?.contextWindow) return;
    let current = true;
    void read(chatId, workflowId).then(model => { if (current) setResolved({ key, model }); }).catch(() => {});
    return () => { current = false; };
  }, [read, chatId, workflowId, key, active, fallback?.contextWindow]);
  if (fallback?.contextWindow) return fallback;
  const fetched = resolved?.key === key ? resolved.model : null;
  if (fallback?.name) {
    // The projection is authoritative for the binding; a resolution whose model
    // differs predates a Settings change, so only matching capacity is adopted.
    return fetched?.name === fallback.name
      ? { name: fallback.name, contextWindow: fetched.contextWindow }
      : fallback;
  }
  return fetched ?? fallback;
}
