// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { captureTimelineAnchor, useTimelineReturn } from "./useTimelineReturn";

afterEach(cleanup);
it("restores the same partially visible message after hidden updates and resized text", async () => {
  const scroll = document.createElement("div");
  scroll.innerHTML = '<div data-timeline-id="first"></div><div data-timeline-id="second"></div>';
  document.body.append(scroll);
  Object.defineProperties(scroll, { scrollHeight: { value: 2000 }, clientHeight: { value: 300 } });
  scroll.scrollTop = 250;
  vi.spyOn(scroll, "getBoundingClientRect").mockReturnValue({ top: 100 } as DOMRect);
  vi.spyOn(scroll.children[0], "getBoundingClientRect").mockReturnValue({ top: 0, bottom: 90 } as DOMRect);
  const bounds = vi.spyOn(scroll.children[1], "getBoundingClientRect").mockReturnValue({ top: 80, bottom: 200 } as DOMRect);
  expect(captureTimelineAnchor(scroll)).toEqual({ id: "second", offset: -20 });
  const following = { current: false }, scrollRef = { current: scroll };
  const scrollToIndex = vi.fn();
  const hook = renderHook(({ active, ids }) => useTimelineReturn(active, scrollRef, ids, following, scrollToIndex), {
    initialProps: { active: true, ids: ["first", "second"] },
  });
  act(() => scroll.dispatchEvent(new Event("scroll")));
  hook.rerender({ active: false, ids: ["first", "second", "new-live-message"] });
  bounds.mockReturnValue({ top: 140, bottom: 380 } as DOMRect);
  expect(scrollToIndex).not.toHaveBeenCalled();
  hook.rerender({ active: true, ids: ["first", "second", "new-live-message"] });
  await waitFor(() => expect(scrollToIndex).toHaveBeenCalledWith(1));
  await waitFor(() => expect(scroll.scrollTop).toBe(310));
  expect(following.current).toBe(false);
  hook.unmount();
  scroll.remove();
});
