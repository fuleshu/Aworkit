import { useCallback, useRef, useState, type RefObject } from "react";
import type { ChatCorePort, RuntimeEvent, RuntimeSnapshot } from "./corePort";
import { mergeCanonicalEvents, withEventSupport } from "./eventWindow";
import { projectSemanticTimeline } from "./activityProjection";
import { conversationFeed, hasEarlierActivity } from "./conversationFeed";

/** Older reads are independent of navigation and cannot replace a newer Chat. */
export function useOlderChatEvents(port: ChatCorePort,
  snapshot: RefObject<RuntimeSnapshot | null>, events: RefObject<RuntimeEvent[]>,
  support: RefObject<RuntimeEvent[]>, generation: RefObject<number>,
  publish: (events: RuntimeEvent[]) => void) {
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
      const visible = conversationFeed(projectSemanticTimeline(withEventSupport(events.current, support.current)), before);
      let earlier: RuntimeEvent[] = [], supporting: RuntimeEvent[] = [];
      do {
        const page = await port.olderEvents(current.chat.chatId, before, current.throughSequence);
        if (generation.current !== owner || request.current !== ticket) return;
        if (!page.events.length || page.window.lastSequence !== before - 1 || page.window.firstSequence >= before || [...page.events, ...page.window.supportingEvents].some(e => e.streamId !== current.chat.chatId || e.branchId !== "main" || e.sequence > current.throughSequence)) throw new Error("Older activity did not join the loaded Chat");
        before = page.window.firstSequence;
        earlier = mergeCanonicalEvents(page.events, earlier, before);
        supporting = withEventSupport(supporting, page.window.supportingEvents);
        // Include any live events received during this read, without making
        // an invisible raw page look like a successful visible prepend.
        const merged = mergeCanonicalEvents(earlier, events.current, before);
        const nextSupport = withEventSupport(support.current, supporting);
        const feed = conversationFeed(projectSemanticTimeline(withEventSupport(merged, nextSupport)), before);
        if (before === 1 || hasEarlierActivity(visible, feed)) {
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
  }, [port, snapshot, events, support, generation, publish]);
  return { loading, error: error?.generation === generation.current ? error.message : null, load };
}
