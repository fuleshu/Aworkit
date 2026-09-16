import { useCallback, useEffect, useLayoutEffect, useRef, type RefObject } from "react";
import { captureTimelineAnchor, alignTimelineAnchor, type TimelineAnchor } from "./useTimelineReturn";

/** Preserve the viewport when earlier rows are prepended. Only intentional
 * upward navigation requests history; initial bottom pin never does. */
export function useHistoryScroll(scroll: RefObject<HTMLDivElement | null>, following: RefObject<boolean>,
  ids: readonly string[], extent: number, hasMore: boolean, loading: boolean,
  load: (() => Promise<void>) | undefined, scrollToIndex: (index: number) => void) {
  const anchor = useRef<{ top: number; extent: number; first: string | undefined; row: TimelineAnchor | null } | null>(null);
  const request = useCallback(async () => {
    if (!load || loading || !hasMore || !scroll.current || anchor.current) return;
    following.current = false;
    anchor.current = { top: scroll.current.scrollTop, extent, first: ids[0], row: captureTimelineAnchor(scroll.current) };
    try { await load(); } finally {
      // Layout effects handle successful prepends before this promise settles.
      requestAnimationFrame(() => { anchor.current = null; });
    }
  }, [load, loading, hasMore, scroll, following, extent, ids]);
  const requestRef = useRef(request);
  requestRef.current = request;
  useLayoutEffect(() => {
    const saved = anchor.current;
    if (!saved || !scroll.current || saved.first === ids[0]) return;
    scroll.current.scrollTop = saved.top + Math.max(0, extent - saved.extent);
    anchor.current = null;
    if (saved.row) {
      const index = ids.indexOf(saved.row.id);
      if (index >= 0) scrollToIndex(index);
      requestAnimationFrame(() => {
        if (scroll.current && saved.row) alignTimelineAnchor(scroll.current, saved.row);
      });
    }
  }, [ids, extent, scroll, scrollToIndex]);
  useEffect(() => {
    const element = scroll.current;
    if (!element) return;
    let previousTop = element.scrollTop;
    const onScroll = () => {
      const upward = element.scrollTop < previousTop;
      previousTop = element.scrollTop;
      if (upward && !following.current && element.scrollTop < 180) void requestRef.current();
    };
    const onWheel = (event: WheelEvent) => { if (event.deltaY < 0 && element.scrollTop < 180) void requestRef.current(); };
    element.addEventListener("scroll", onScroll, { passive: true });
    element.addEventListener("wheel", onWheel, { passive: true });
    return () => { element.removeEventListener("scroll", onScroll); element.removeEventListener("wheel", onWheel); };
  }, [scroll, following]);
  return request;
}
