import { useCallback, useRef, useState, type RefObject } from "react";
import type { ChatCorePort, RuntimeEvent, RuntimeSnapshot } from "./corePort";
import type { TimelineItem } from "./types";
import { mergeCanonicalEvents, withEventSupport } from "./eventWindow";
import { projectSemanticTimeline } from "./activityProjection";
import { conversationFeed } from "./conversationFeed";

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
      // Baseline: what the transcript shows before this read. Only the shown
      // ids and the oldest shown sequence matter from here on.
      const baseline = filterItems(conversationFeed(projectSemanticTimeline(withEventSupport(events.current, support.current)), before));
      const shownIds = new Set(baseline.map(item => item.id));
      const earliest = baseline[0]?.sequence ?? Number.POSITIVE_INFINITY;
      const pages: RuntimeEvent[][] = [];
      const supports: RuntimeEvent[][] = [];
      let shown = 0, scanned = 0;
      do {
        const page = await port.olderEvents(current.chat.chatId, before, current.throughSequence);
        if (generation.current !== owner || request.current !== ticket) return;
        if (!page.events.length || page.window.lastSequence !== before - 1 || page.window.firstSequence >= before || [...page.events, ...page.window.supportingEvents].some(e => e.streamId !== current.chat.chatId || e.branchId !== "main" || e.sequence > current.throughSequence)) throw new Error("Older activity did not join the loaded Chat");
        scanned += 1;
        before = page.window.firstSequence;
        pages.push(page.events);
        supports.push(page.window.supportingEvents);
        // Classify only this page. Merging and re-projecting the whole loaded
        // range on every page made one read quadratic, which is what made the
        // filtered prefetch slow. A span that crosses a page edge can only make
        // this count smaller, and the page budget bounds the difference.
        const pageFeed = filterItems(conversationFeed(projectSemanticTimeline(withEventSupport(page.events, page.window.supportingEvents)), before));
        for (const item of pageFeed) {
          if (shownIds.has(item.id)) continue;
          shownIds.add(item.id);
          if ((item.sequence ?? 0) < earliest) shown += 1;
        }
        // Read on until enough *shown* entries arrived; the page budget keeps
        // one read bounded and the next scroll continues from where it stopped.
        if (before === 1 || shown >= olderPageTarget || scanned >= OLDER_PAGE_BUDGET) {
          // One merge and one support union at the end, never per page.
          support.current = withEventSupport(support.current, supports.flat());
          publish(mergeCanonicalEvents(pages.flat(), events.current, before));
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
