import { useCallback, useRef, useState, type RefObject } from "react";
import type { ChatCorePort, RuntimeEvent, RuntimeSnapshot } from "./corePort";
import type { TimelineItem } from "./types";
import { mergeCanonicalEvents, withEventSupport } from "./eventWindow";
import { projectSemanticTimeline } from "./activityProjection";
import { conversationFeed, countEarlierActivity } from "./conversationFeed";

/**
 * Shown older entries one read aims to reveal when the transcript is filtered.
 *
 * With thinking/tools hidden a raw 128-event page can produce a single shown
 * entry, so stopping at the first one made fast scrolling crawl. The unfiltered
 * path keeps its default target of one and is unchanged.
 */
export const FILTERED_OLDER_TARGET = 32;

/** Raw pages one read may scan before it yields, so no hidden stretch stalls it. */
const OLDER_PAGE_BUDGET = 24;

/** Older reads are independent of navigation and cannot replace a newer Chat. */
export function useOlderChatEvents(port: ChatCorePort,
  snapshot: RefObject<RuntimeSnapshot | null>, events: RefObject<RuntimeEvent[]>,
  support: RefObject<RuntimeEvent[]>, generation: RefObject<number>,
  publish: (events: RuntimeEvent[]) => void,
  filterItems: (items: readonly TimelineItem[]) => TimelineItem[],
  olderPageTarget = 1) {
  const request = useRef(0);
  const activeGeneration = useRef<number | null>(null);
  const [busy, setBusy] = useState<{ generation: number; active: boolean }>({ generation: -1, active: false });
  const [error, setError] = useState<{ generation: number; message: string } | null>(null);
  const loading = busy.active && busy.generation === generation.current;
  const load = useCallback(async () => {
    const current = snapshot.current;
    let before = events.current[0]?.sequence ?? 1;
    const owner = generation.current;
    if (!current || !port.olderEvents || before <= 1 || activeGeneration.current === owner) return;
    const ticket = ++request.current;
    activeGeneration.current = owner;
    setBusy({ generation: owner, active: true }); setError(null);
    try {
      const visible = filterItems(conversationFeed(projectSemanticTimeline(withEventSupport(events.current, support.current)), before));
      let earlier: RuntimeEvent[] = [], supporting: RuntimeEvent[] = [];
      let scanned = 0;
      do {
        const page = await port.olderEvents(current.chat.chatId, before, current.throughSequence);
        if (generation.current !== owner || request.current !== ticket) return;
        if (!page.events.length || page.window.lastSequence !== before - 1 || page.window.firstSequence >= before || [...page.events, ...page.window.supportingEvents].some(e => e.streamId !== current.chat.chatId || e.branchId !== "main" || e.sequence > current.throughSequence)) throw new Error("Older activity did not join the loaded Chat");
        scanned += 1;
        before = page.window.firstSequence;
        earlier = mergeCanonicalEvents(page.events, earlier, before);
        supporting = withEventSupport(supporting, page.window.supportingEvents);
        // Include any live events received during this read, without making
        // an invisible raw page look like a successful visible prepend.
        const merged = mergeCanonicalEvents(earlier, events.current, before);
        const nextSupport = withEventSupport(support.current, supporting);
        const feed = filterItems(conversationFeed(projectSemanticTimeline(withEventSupport(merged, nextSupport)), before));
        // Read on until enough *shown* entries arrived: a page the output filter
        // hides adds nothing visible, and stopping at the first visible entry
        // made fast scrolling advance one entry per read. The page budget keeps
        // one read bounded; the next scroll continues from where it stopped.
        const gained = countEarlierActivity(visible, feed);
        if (before === 1 || gained >= olderPageTarget || scanned >= OLDER_PAGE_BUDGET) {
          support.current = nextSupport;
          publish(merged);
          break;
        }
      } while (before > 1);
    } catch (failure) {
      if (generation.current === owner) setError({ generation: owner, message: failure instanceof Error ? failure.message : String(failure) });
    } finally {
      if (request.current === ticket) { activeGeneration.current = null; setBusy({ generation: owner, active: false }); }
    }
  }, [port, snapshot, events, support, generation, publish, filterItems, olderPageTarget]);
  return { loading, error: error?.generation === generation.current ? error.message : null, load };
}
