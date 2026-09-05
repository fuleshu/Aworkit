import { useLayoutEffect, useRef, type RefObject } from "react";

export interface TimelineAnchor { readonly id: string; readonly offset: number }

/** Anchors to a message, so committed text-size changes do not move a manually scrolled view. */
export function captureTimelineAnchor(scroll: HTMLElement): TimelineAnchor | null {
  const top = scroll.getBoundingClientRect().top;
  const row = [...scroll.querySelectorAll<HTMLElement>("[data-timeline-id]")]
    .find(element => element.getBoundingClientRect().bottom > top);
  return row ? { id: row.dataset.timelineId!, offset: row.getBoundingClientRect().top - top } : null;
}

export function alignTimelineAnchor(scroll: HTMLElement, anchor: TimelineAnchor): void {
  const row = [...scroll.querySelectorAll<HTMLElement>("[data-timeline-id]")]
    .find(element => element.dataset.timelineId === anchor.id);
  if (row) scroll.scrollTop += row.getBoundingClientRect().top - scroll.getBoundingClientRect().top - anchor.offset;
}

/** Hidden runtime facts continue updating; only the visible timeline measures/restores its view. */
export function useTimelineReturn(
  active: boolean,
  scrollRef: RefObject<HTMLDivElement | null>,
  ids: readonly string[],
  following: RefObject<boolean>,
  scrollToIndex: (index: number) => void,
) {
  const anchor = useRef<TimelineAnchor | null>(null);
  const wasActive = useRef(active);
  const restoring = useRef(false);
  const current = useRef({ ids, scrollToIndex });
  current.current = { ids, scrollToIndex };
  useLayoutEffect(() => {
    const returning = active && !wasActive.current;
    wasActive.current = active;
    const scroll = scrollRef.current;
    if (!active || !scroll) return;
    const frames = new Set<number>();
    const frame = (callback: () => void) => {
      const id = window.requestAnimationFrame(() => { frames.delete(id); callback(); });
      frames.add(id);
    };
    const remember = () => {
      if (restoring.current) return;
      if (scroll.scrollHeight - scroll.scrollTop - scroll.clientHeight < 48) { anchor.current = null; return; }
      anchor.current = captureTimelineAnchor(scroll) ?? anchor.current;
      // Virtual rows may be replaced after the scroll event; capture the committed row set too.
      frame(() => { if (!restoring.current) anchor.current = captureTimelineAnchor(scroll) ?? anchor.current; });
    };
    if (returning && !following.current && anchor.current) {
      const saved = anchor.current;
      const index = current.current.ids.indexOf(saved.id);
      if (index >= 0) {
        restoring.current = true;
        frame(() => {
          current.current.scrollToIndex(index);
          frame(() => {
            alignTimelineAnchor(scroll, saved);
            following.current = false;
            scroll.dataset.followLatest = "false";
            restoring.current = false;
          });
        });
      }
    }
    scroll.addEventListener("scroll", remember, { passive: true });
    return () => {
      scroll.removeEventListener("scroll", remember);
      frames.forEach(window.cancelAnimationFrame);
      restoring.current = false;
    };
  }, [active, scrollRef, following]);
  return restoring;
}
