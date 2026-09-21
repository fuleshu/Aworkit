import { useCallback, useEffect, useMemo, useRef, useState } from "react";
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

/** Shared empty scope keeps memo identity stable between tab switches. */
const NO_EVENTS: RuntimeEvent[] = [];

interface ChildScope {
  readonly chatId: string;
  readonly childId: string;
  readonly events: readonly RuntimeEvent[];
  readonly support: readonly RuntimeEvent[];
  /** Highest sequence still unscanned; the next page ends just below it. */
  readonly cursor: number;
  readonly exhausted: boolean;
  /** This scope already pulled its own evidence from the core. */
  readonly loaded: boolean;
}

export interface SubagentFeed {
  readonly events: readonly RuntimeEvent[];
  readonly support: readonly RuntimeEvent[];
  readonly firstSequence: number;
  readonly hasOlder: boolean;
  readonly olderLoading: boolean;
  readonly olderError: string | null;
  /** The scope has not yet pulled its own evidence from the core. */
  readonly loading: boolean;
  readonly loadOlder: () => Promise<void>;
}

/**
 * One child's evidence: the child-tagged facts already loaded by the Chat feed
 * plus the child's own bounded pages.
 *
 * The Chat feed is a *recent* window, so a child's earlier facts — including the
 * `span.started` records its cards need — are often outside it. The scope
 * therefore pulls its own newest page as soon as it becomes active, and keeps
 * stepping the raw cursor back on demand until an earlier child activity
 * appears or history is exhausted. It never fetches a contiguous range of the
 * Run, because a child scope is sparse by construction.
 */
export function useSubagentFeed(
  port: ChatCorePort,
  chatId: string,
  childId: string,
  throughSequence: number,
  liveEvents: readonly RuntimeEvent[],
  active: boolean,
): SubagentFeed {
  const [scopes, setScopes] = useState<ChildScope>({
    chatId: "",
    childId: "",
    events: NO_EVENTS,
    support: NO_EVENTS,
    cursor: 0,
    exhausted: true,
    loaded: false,
  });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const ticket = useRef(0);
  const pulling = useRef(false);

  const scope: ChildScope =
    childId !== "" && scopes.chatId === chatId && scopes.childId === childId
      ? scopes
      : {
          chatId,
          childId,
          events: NO_EVENTS,
          support: NO_EVENTS,
          cursor: throughSequence,
          exhausted: throughSequence < 1,
          loaded: false,
        };
  const loaded = scope.loaded;

  const live = useMemo(
    () => subagentChildEvents(liveEvents, childId),
    [liveEvents, childId],
  );
  const events = useMemo(
    () => mergeChildEvents(scope.events, live),
    [scope.events, live],
  );
  const hasOlder = active && !scope.exhausted && scope.cursor >= 1;

  /**
   * Steps the raw cursor back from the scope's current position until a page
   * yields a child fact or history is exhausted.
   */
  const pull = useCallback(
    async (initial: boolean) => {
      if (!active || childId === "" || port.subagentEvents === undefined) return;
      if (pulling.current) return;
      if (!initial && (!hasOlder || busy)) return;
      pulling.current = true;
      const mine = ++ticket.current;
      setBusy(true);
      setError(null);
      try {
        let cursor = initial ? throughSequence : scope.cursor;
        let collected: RuntimeEvent[] = [];
        let support = scope.support;
        let exhausted = cursor < 1;
        while (cursor >= 1) {
          // `beforeSequence` is exclusive, so ask for the page ending at cursor.
          const page = await port.subagentEvents(
            chatId,
            childId,
            cursor + 1,
            throughSequence,
          );
          if (ticket.current !== mine) return;
          collected = mergeChildEvents(collected, page.events);
          support = withEventSupport(support, page.window.supportingEvents);
          exhausted = !page.window.hasMore;
          cursor = page.window.firstSequence - 1;
          if (page.events.length > 0) break;
        }
        setScopes((current) => {
          const merged =
            current.chatId === chatId && current.childId === childId
              ? current
              : scope;
          return {
            chatId,
            childId,
            events: mergeChildEvents(merged.events, collected),
            support,
            cursor: Math.max(cursor, 0),
            exhausted,
            loaded: true,
          };
        });
      } catch (failure) {
        // A failed pull still settles the scope, so a broken read can never
        // become an unbounded retry loop; the error stays visible.
        if (ticket.current === mine) {
          setError(failure instanceof Error ? failure.message : String(failure));
          setScopes((current) =>
            current.chatId === chatId && current.childId === childId
              ? { ...current, loaded: true }
              : {
                  chatId,
                  childId,
                  events: scope.events,
                  support: scope.support,
                  cursor: scope.cursor,
                  exhausted: scope.exhausted,
                  loaded: true,
                },
          );
        }
      } finally {
        if (ticket.current === mine) {
          pulling.current = false;
          setBusy(false);
        }
      }
    },
    [active, busy, chatId, childId, hasOlder, port, scope, throughSequence],
  );

  // A remembered child tab must not depend on the user scrolling a list that
  // has nothing in it: the scope hydrates itself once it becomes active.
  useEffect(() => {
    if (!active || childId === "" || loaded) return;
    void pull(true);
  }, [active, childId, loaded, pull]);

  const loadOlder = useCallback(async () => {
    await pull(false);
  }, [pull]);

  return {
    events,
    support: scope.support,
    firstSequence: events[0]?.sequence ?? 1,
    hasOlder,
    olderLoading: busy,
    olderError: error,
    loading: active && childId !== "" && !loaded,
    loadOlder,
  };
}
