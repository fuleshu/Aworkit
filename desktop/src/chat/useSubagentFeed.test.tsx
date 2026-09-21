// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { subagentChildEvents, useSubagentFeed } from "./useSubagentFeed";
import type { ChatCorePort, ChatEventPage, RuntimeEvent } from "./corePort";

afterEach(cleanup);

const event = (
  sequence: number,
  kind: string,
  payload: Record<string, unknown>,
): RuntimeEvent => ({
  schemaVersion: 1,
  streamId: "chat.subagents",
  branchId: "main",
  sequence,
  eventId: `event.${sequence}`,
  kind,
  payload,
});

const childFact = (sequence: number, childId: string): RuntimeEvent =>
  event(sequence, "span.content_delta", {
    spanId: `span.model.${childId}`,
    subagentChildId: childId,
    append: `part ${sequence}`,
  });

function page(
  events: readonly RuntimeEvent[],
  firstSequence: number,
): ChatEventPage {
  return {
    window: {
      firstSequence,
      lastSequence: events.at(-1)?.sequence ?? firstSequence,
      headSequence: 10,
      hasMore: firstSequence > 1,
      supportingEvents: [],
    },
    events: [...events],
  };
}

function port(
  subagentEvents: NonNullable<ChatCorePort["subagentEvents"]>,
): ChatCorePort {
  return {
    async snapshot() {
      throw new Error("unused");
    },
    async command(intent) {
      return {
        commandId: intent.commandId,
        accepted: false,
        currentVersion: 1,
        reason: "unused",
      };
    },
    subagentEvents,
  };
}

describe("child-scoped evidence feed", () => {
  it("filters raw events to exactly one child scope", () => {
    const live = [
      childFact(1, "child.a"),
      childFact(2, "child.b"),
      event(3, "message.user", { body: "parent" }),
      childFact(4, "child.a"),
    ];
    expect(subagentChildEvents(live, "child.a").map((item) => item.sequence)).toEqual([
      1, 4,
    ]);
    expect(subagentChildEvents(live, "child.missing")).toEqual([]);
    expect(subagentChildEvents(live, "")).toEqual([]);
  });

  it("hydrates its own page on activation and pages back until history ends", async () => {
    const live = [childFact(8, "child.a"), childFact(9, "child.a")];
    const calls: number[] = [];
    const subagentEvents = vi.fn(
      async (
        _chatId: string,
        _childId: string,
        beforeSequence: number,
      ): Promise<ChatEventPage> => {
        calls.push(beforeSequence);
        if (calls.length === 1) {
          // The newest raw page scanned only other scopes; the cursor advances
          // without yielding an activity.
          return page([], 6);
        }
        if (calls.length === 2) {
          return page([childFact(3, "child.a"), childFact(5, "child.a")], 3);
        }
        return page([], 1);
      },
    );
    const { result, rerender } = renderHook(
      ({ liveEvents }: { readonly liveEvents: readonly RuntimeEvent[] }) =>
        useSubagentFeed(
          port(subagentEvents),
          "chat.subagents",
          "child.a",
          10,
          liveEvents,
          true,
        ),
      { initialProps: { liveEvents: live } },
    );
    // The scope asks for the page ending at the head, not only on scroll-back.
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(calls).toEqual([11, 6]);
    expect(result.current.events.map((item) => item.sequence)).toEqual([
      3, 5, 8, 9,
    ]);
    expect(result.current.firstSequence).toBe(3);
    expect(result.current.hasOlder).toBe(true);
    expect(result.current.olderError).toBeNull();

    await act(async () => {
      await result.current.loadOlder();
    });
    expect(calls).toEqual([11, 6, 3]);
    expect(result.current.hasOlder).toBe(false);

    // Live delivery extends the same scope without refetching.
    rerender({ liveEvents: [...live, childFact(10, "child.a")] });
    expect(result.current.events.map((item) => item.sequence)).toEqual([
      3, 5, 8, 9, 10,
    ]);
    expect(subagentEvents).toHaveBeenCalledTimes(3);
  });

  it("settles a failed read instead of retrying it forever", async () => {
    const subagentEvents = vi.fn().mockRejectedValue(new Error("read failed"));
    const { result } = renderHook(() =>
      useSubagentFeed(
        port(subagentEvents),
        "chat.subagents",
        "child.a",
        10,
        [childFact(9, "child.a")],
        true,
      ),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.olderError).toBe("read failed");
    expect(subagentEvents).toHaveBeenCalledTimes(1);
    // The live scope stays usable while the older read is reported.
    expect(result.current.events.map((item) => item.sequence)).toEqual([9]);
  });

  it("never reads while the child tab is inactive", async () => {
    const subagentEvents = vi.fn();
    const { result } = renderHook(() =>
      useSubagentFeed(
        port(subagentEvents),
        "chat.subagents",
        "child.a",
        10,
        [],
        false,
      ),
    );
    expect(result.current.hasOlder).toBe(false);
    expect(result.current.loading).toBe(false);
    await act(async () => {
      await result.current.loadOlder();
    });
    expect(subagentEvents).not.toHaveBeenCalled();
  });
});
