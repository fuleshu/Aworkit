// @vitest-environment jsdom
import { renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useChatCompletionNotices } from "./useChatCompletionNotices";

afterEach(() => vi.restoreAllMocks());

const titles = new Map([
  ["chat.a", "Alpha"],
  ["chat.b", "Beta"],
]);
const titleFor = (chatId: string) => titles.get(chatId);

describe("useChatCompletionNotices", () => {
  it("marks a Chat unread when its Run finishes off screen and clears it when opened", () => {
    const { result, rerender } = renderHook(
      ({ active, selected }: { active: readonly string[]; selected: string | undefined }) =>
        useChatCompletionNotices(active, selected, titleFor, () => {}),
      { initialProps: { active: ["chat.a", "chat.b"], selected: "chat.a" } },
    );
    expect([...result.current.unreadChatIds]).toEqual([]);

    // chat.b finishes while chat.a is the visible Chat.
    rerender({ active: ["chat.a"], selected: "chat.a" });
    expect([...result.current.unreadChatIds]).toEqual(["chat.b"]);

    // Opening chat.b clears its marker.
    rerender({ active: ["chat.a"], selected: "chat.b" });
    expect([...result.current.unreadChatIds]).toEqual([]);
  });

  it("does not mark the visible Chat and notifies only an unfocused window", () => {
    const notify = vi.fn();
    vi.spyOn(document, "hasFocus").mockReturnValue(false);
    const { result, rerender } = renderHook(
      ({ active, selected }: { active: readonly string[]; selected: string | undefined }) =>
        useChatCompletionNotices(active, selected, titleFor, notify),
      { initialProps: { active: ["chat.a", "chat.b"], selected: "chat.a" } },
    );

    rerender({ active: [], selected: "chat.a" });

    // The visible Chat gets no marker; the background one does. Both completions
    // raise an OS notification because the window is not focused.
    expect([...result.current.unreadChatIds]).toEqual(["chat.b"]);
    expect(notify).toHaveBeenCalledWith(
      "Aworkit finished a reply",
      "Alpha has a new reply.",
    );
    expect(notify).toHaveBeenCalledWith(
      "Aworkit finished a reply",
      "Beta has a new reply.",
    );
  });

  it("stays quiet when the window is focused", () => {
    const notify = vi.fn();
    vi.spyOn(document, "hasFocus").mockReturnValue(true);
    const { result, rerender } = renderHook(
      ({ active, selected }: { active: readonly string[]; selected: string | undefined }) =>
        useChatCompletionNotices(active, selected, titleFor, notify),
      { initialProps: { active: ["chat.b"], selected: "chat.a" } },
    );
    rerender({ active: [], selected: "chat.a" });
    expect([...result.current.unreadChatIds]).toEqual(["chat.b"]);
    expect(notify).not.toHaveBeenCalled();
  });
});
