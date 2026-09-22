// @vitest-environment jsdom
/*
 * The interaction contracts around the virtualized transcript: following
 * the tail, restoring the reading anchor after a return, and deciding a
 * click is a selection rather than an action. One jsdom environment covers
 * all three instead of three.
 */
import { act, cleanup, fireEvent, render, renderHook, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { isSelectionClick } from "./selectionClick";
import { useTimelineFollow } from "./useTimelineFollow";
import { captureTimelineAnchor, useTimelineReturn } from "./useTimelineReturn";

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

it("gives nested native and custom controls priority and selects only unclaimed content", () => {
  const select = vi.fn();
  const action = vi.fn();
  render(
    <section tabIndex={0} onClick={event => { if (isSelectionClick(event)) select(); }}>
      <button onClick={action}><svg><title>Nested icon</title></svg></button>
      <details><summary><strong>Disclosure</strong></summary>Hidden content</details>
      <label>Input label<input aria-label="Field" /></label>
      <span role="button" tabIndex={0} onClick={action}>Custom button</span>
      <span onClick={event => { event.preventDefault(); action(); }}>Claimed click</span>
      <span onClick={event => { event.stopPropagation(); action(); }}>Stopped click</span>
      <p>Plain content</p>
    </section>,
  );
  for (const label of ["Nested icon", "Disclosure", "Input label", "Custom button", "Claimed click", "Stopped click"]) {
    fireEvent.click(screen.getByText(label));
  }
  fireEvent.click(screen.getByRole("textbox"));
  expect(select).not.toHaveBeenCalled();
  expect(action).toHaveBeenCalledTimes(4);
  fireEvent.click(screen.getByText("Plain content"));
  expect(select).toHaveBeenCalledTimes(1);
});
