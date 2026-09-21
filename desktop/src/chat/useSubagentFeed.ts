import { useCallback, useMemo, useRef, useState } from "react";
import type { ChatCorePort, RuntimeEvent } from "./corePort";
import { withEventSupport } from "./eventWindow";

/** Every child fact carries its durable identity, so a child scope is a filter. */
export function subagentChildEvents(
  events: readonly RuntimeEvent[],
  childId: string,
): RuntimeEvent[] {
  return events.filter((event) => {
    const payload = event.payload;
    return (
      typeof payload === "object" &&
      payload !== null &&
      (payload as { subagentChildId?: unknown }).subagentChildId === childId
    );
  });
}

/** Sequence-keyed merge of a non-contiguous filtered scope. */
function mergeChildEvents(
  current: readonly RuntimeEvent[],
  incoming: readonly RuntimeEvent[],
): RuntimeEvent[] {
  const bySequence = new Map<number, RuntimeEvent>();
  for (const event of [...current, ...incoming]) {
    const existing = bySequence.get(event.sequence);
    if (
      existing !== undefined &&
      existing !== event &&
      JSON.stringify(existing) !== JSON.stringify(event)
    ) {
      throw new Error(`canonical event conflict at sequence ${event.sequence}`);
    }
    bySequence.set(event.sequence, event);
  }
  return [...bySequence.values()].sort((left, right) => left.sequence - right.sequence);
}

export interface SubagentFeed {
  readonly events: readonly RuntimeEvent[];
  readonly support: readonly RuntimeEvent[];
  readonly firstSequence: number;
  readonly hasOlder: boolean;
  readonly olderLoading: boolean;
  readonly olderError: string | null;
  readonly loadOlder: () => Promise<void>;
}

/** Shared empty scope keeps memo identity stable between tab switches. */
const NO_EVENTS: RuntimeEvent[] = [];

/**
 * One child's evidence: the child-tagged facts already loaded by the Chat feed
 * plus older child pages fetched on demand with the same bounded window
 * semantics. It never fetches a contiguous range of the Run, because a child
 * scope is sparse by construction.
 */
export function useSubagentFeed(
  port: ChatCorePort,
  chatId: string,
  childId: string,
  throughSequence: number,
  liveEvents: readonly RuntimeEvent[],
  active: boolean,
): SubagentFeed {
  const [older, setOlder] = useState<{
    chatId: string;
    childId: string;
    events: RuntimeEvent[];
    support: RuntimeEvent[];
    cursor: number;
    exhausted: boolean;
  }>({ chatId, childId, events: [], support: [], cursor: throughSequence, exhausted: false });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const ticket = useRef(0);

  // A different Chat or child starts from that scope's own bounded window.
  const materialized =
    older.chatId === chatId && older.childId === childId
      ? older
      : {
          chatId,
          childId,
          events: NO_EVENTS,
          support: NO_EVENTS,
          cursor: throughSequence,
          exhausted: false,
        };

  const live = useMemo(
    () => subagentChildEvents(liveEvents, childId),
    [liveEvents, childId],
  );
  const events = useMemo(
    () => mergeChildEvents(materialized.events, live),
    [materialized.events, live],
  );
  const hasOlder = active && !materialized.exhausted && materialized.cursor > 1;

  const loadOlder = useCallback(async () => {
    if (!active || !hasOlder || busy || port.subagentEvents === undefined) return;
    const mine = ++ticket.current;
    setBusy(true);
    setError(null);
    try {
      let cursor = materialized.cursor;
      let collected: RuntimeEvent[] = [];
      let support = materialized.support;
      // A page can be entirely other scopes; keep stepping back until an
      // earlier child activity appears or history is exhausted.
      while (cursor > 1) {
        const page = await port.subagentEvents(chatId, childId, cursor, throughSequence);
        if (ticket.current !== mine) return;
        collected = mergeChildEvents(collected, page.events);
        support = withEventSupport(support, page.window.supportingEvents);
        cursor = page.window.firstSequence;
        if (page.events.length > 0) break;
      }
      setOlder((current) => {
        if (current.chatId !== chatId || current.childId !== childId) return current;
        return {
          chatId,
          childId,
          events: mergeChildEvents(current.events, collected),
          support,
          cursor,
          exhausted: cursor <= 1,
        };
      });
    } catch (failure) {
      if (ticket.current === mine) {
        setError(failure instanceof Error ? failure.message : String(failure));
      }
    } finally {
      if (ticket.current === mine) setBusy(false);
    }
  }, [active, busy, chatId, childId, hasOlder, materialized.cursor, materialized.support, port, throughSequence]);

  return {
    events,
    support: materialized.support,
    firstSequence: events[0]?.sequence ?? 1,
    hasOlder,
    olderLoading: busy,
    olderError: error,
    loadOlder,
  };
}
