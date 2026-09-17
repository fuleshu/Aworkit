import { useCallback, useEffect, useLayoutEffect, useRef, useState, type RefObject } from "react";
import { captureTimelineAnchor, alignTimelineAnchor, type TimelineAnchor } from "./useTimelineReturn";

/** Preserve the viewport when earlier rows are prepended. Only intentional
 * upward navigation requests history; initial bottom pin never does. */
export function useHistoryScroll(scroll: RefObject<HTMLDivElement | null>, following: RefObject<boolean>,
  ids: readonly string[], extent: number, hasMore: boolean, loading: boolean,
  load: (() => Promise<void>) | undefined, scrollToIndex: (index: number) => void) {
  const anchor = useRef<{ top: number; extent: number; first: string | undefined; row: TimelineAnchor | null; settled: boolean } | null>(null);
  const [completed, setCompleted] = useState(0);
  const restoring = useRef(false);
  const frame = useRef<number | null>(null);
  useEffect(() => () => { if (frame.current !== null) cancelAnimationFrame(frame.current); }, []);
  const request = useCallback(async () => {
    if (!load || loading || !hasMore || !scroll.current || anchor.current) return;
    following.current = false;
    anchor.current = { top: scroll.current.scrollTop, extent, first: ids[0], row: captureTimelineAnchor(scroll.current), settled: false };
    try { await load(); } finally {
      // The async result can settle before React commits its new rows. Keep
      // the anchor until that commit, instead of clearing it on the next frame.
      if (anchor.current) anchor.current.settled = true;
      setCompleted(value => value + 1);
    }
  }, [load, loading, hasMore, scroll, following, extent, ids]);
  const requestRef = useRef(request);
  requestRef.current = request;
  useLayoutEffect(() => {
    const saved = anchor.current;
    if (!saved?.settled || !scroll.current) return;
    anchor.current = null;
    if (saved.first === ids[0]) return;
    restoring.current = true;
    scroll.current.scrollTop = saved.top + Math.max(0, extent - saved.extent);
    if (saved.row) {
      const index = ids.indexOf(saved.row.id);
      if (index >= 0) scrollToIndex(index);
    }
    // Virtual rows first use estimates, then ResizeObserver measurements.
    // Align the same activity through those commits, never a parent container.
    let remaining = 3;
    const align = () => {
      if (scroll.current && saved.row) alignTimelineAnchor(scroll.current, saved.row);
      if (--remaining > 0) frame.current = requestAnimationFrame(align);
      else { frame.current = null; restoring.current = false; }
    };
    if (frame.current !== null) cancelAnimationFrame(frame.current);
    frame.current = requestAnimationFrame(align);
  }, [ids, extent, scroll, scrollToIndex, completed]);
  useEffect(() => {
    const element = scroll.current;
    if (!element) return;
    let previousTop = element.scrollTop;
    const onScroll = () => {
      const upward = element.scrollTop < previousTop;
      previousTop = element.scrollTop;
      if (upward && !restoring.current && !following.current && element.scrollTop < 180) void requestRef.current();
    };
    const onWheel = (event: WheelEvent) => { if (event.deltaY < 0 && element.scrollTop < 180) void requestRef.current(); };
    element.addEventListener("scroll", onScroll, { passive: true });
    element.addEventListener("wheel", onWheel, { passive: true });
    return () => { element.removeEventListener("scroll", onScroll); element.removeEventListener("wheel", onWheel); };
  }, [scroll, following]);
  return request;
}
