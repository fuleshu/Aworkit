// @vitest-environment jsdom
import { act, cleanup, fireEvent, renderHook, waitFor } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { useTimelineFollow } from "./useTimelineFollow";

afterEach(cleanup);
it("follows layout growth but respects even a small intentional scroll until returning down", async () => {
  const scroll = document.createElement("div");
  scroll.innerHTML = '<div class="virtual-timeline"></div>';
  document.body.append(scroll);
  let height = 1000, top = 800;
  Object.defineProperties(scroll, {
    scrollHeight: { get: () => height }, clientHeight: { value: 200 },
    scrollTop: { get: () => top, set: value => { top = Math.max(0, Math.min(value, height - 200)); } },
  });
  const following = { current: true };
  const { result, unmount } = renderHook(() => useTimelineFollow(true, { current: scroll }, following, { current: false }));
  act(() => { height = 1500; fireEvent.scroll(scroll); result.current.current(); });
  await waitFor(() => expect(top).toBe(1300));
  act(() => { fireEvent.wheel(scroll, { deltaY: -5 }); scroll.scrollTop = 1295; fireEvent.scroll(scroll); });
  expect(following.current).toBe(false);
  act(() => { height = 1600; result.current.current(); });
  await new Promise(resolve => setTimeout(resolve, 40));
  expect(top).toBe(1295);
  act(() => { fireEvent.wheel(scroll, { deltaY: 1000 }); scroll.scrollTop = 1400; fireEvent.scroll(scroll); });
  expect(following.current).toBe(true);
  act(() => { height = 1800; result.current.current(); });
  await waitFor(() => expect(top).toBe(1600));
  unmount(); scroll.remove();
});
