import { useLayoutEffect, useRef, type RefObject } from "react";

/** Follow rendered growth, including virtual measurements, streaming cards and image loads.
 * Growing content must not be mistaken for an intentional scroll away from the bottom.
 */
export function useTimelineFollow(active: boolean, scrollRef: RefObject<HTMLDivElement | null>,
  following: RefObject<boolean>, restoring: RefObject<boolean>) {
  const schedule = useRef(() => {});
  useLayoutEffect(() => {
    const scroll = scrollRef.current;
    if (!active || !scroll) return;
    let frame: number | null = null;
    let previousTop = scroll.scrollTop;
    let mayReattach = true;
    const setFollowing = (value: boolean) => {
      following.current = value;
      scroll.dataset.followLatest = String(value);
    };
    const follow = () => {
      if (frame !== null) return;
      frame = requestAnimationFrame(() => {
        frame = null;
        if (following.current && !restoring.current) scroll.scrollTop = scroll.scrollHeight;
        previousTop = scroll.scrollTop;
      });
    };
    schedule.current = follow;
    const onScroll = () => {
      if (restoring.current) return;
      if (!following.current && Math.abs(scroll.scrollTop - previousTop) > 1) {
        mayReattach = scroll.scrollTop > previousTop;
      }
      previousTop = scroll.scrollTop;
      // Native wheel scrolling can stop a few physical pixels short at fractional DPI.
      const atEnd = scroll.scrollHeight - scroll.scrollTop - scroll.clientHeight < 24;
      if (atEnd && mayReattach) setFollowing(true);
    };
    const wheel = (event: WheelEvent) => {
      mayReattach = event.deltaY > 0;
      if (event.deltaY < 0) setFollowing(false);
    };
    const pointer = (event: PointerEvent) => {
      const bounds = scroll.getBoundingClientRect();
      const gutter = scroll.offsetWidth - scroll.clientWidth;
      if (event.clientX >= bounds.right - gutter && event.clientX <= bounds.right) {
        mayReattach = false;
        setFollowing(false);
      }
    };
    const key = (event: KeyboardEvent) => {
      if (event.target !== scroll) return;
      if (["ArrowUp", "PageUp", "Home"].includes(event.key)) { mayReattach = false; setFollowing(false); }
      if (["ArrowDown", "PageDown"].includes(event.key)) mayReattach = true;
      if (event.key === "End") { mayReattach = true; setFollowing(true); follow(); }
    };
    // Touch scrolling can coincide with a resize, so record its direction explicitly.
    let touchY = 0;
    const touchStart = (event: TouchEvent) => { touchY = event.touches[0]?.clientY ?? 0; };
    const touchMove = (event: TouchEvent) => {
      const y = event.touches[0]?.clientY ?? touchY;
      mayReattach = y < touchY;
      if (y > touchY) setFollowing(false);
      touchY = y;
    };
    const observer = new ResizeObserver(follow);
    observer.observe(scroll);
    const extent = scroll.querySelector(".virtual-timeline");
    if (extent) observer.observe(extent);
    const mutations = new MutationObserver(follow);
    mutations.observe(scroll, { subtree: true, childList: true, characterData: true });
    scroll.addEventListener("scroll", onScroll, { passive: true });
    scroll.addEventListener("wheel", wheel, { passive: true });
    scroll.addEventListener("pointerdown", pointer, { passive: true });
    scroll.addEventListener("keydown", key);
    scroll.addEventListener("touchstart", touchStart, { passive: true });
    scroll.addEventListener("touchmove", touchMove, { passive: true });
    follow();
    return () => {
      observer.disconnect(); mutations.disconnect();
      scroll.removeEventListener("scroll", onScroll); scroll.removeEventListener("wheel", wheel);
      scroll.removeEventListener("pointerdown", pointer);
      scroll.removeEventListener("keydown", key); scroll.removeEventListener("touchstart", touchStart);
      scroll.removeEventListener("touchmove", touchMove);
      if (frame !== null) cancelAnimationFrame(frame);
      schedule.current = () => {};
    };
  }, [active, scrollRef, following, restoring]);
  return schedule;
}
