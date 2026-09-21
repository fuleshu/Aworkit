// @vitest-environment jsdom
import { act, cleanup, renderHook } from "@testing-library/react";
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
  });

  it("pages back until an earlier child activity appears or history ends", async () => {
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
          // A page that only scanned other scopes advances without yielding.
          return page([], 6);
        }
        if (calls.length === 2) {
          return page([childFact(3, "child.a"), childFact(5, "child.a")], 3);
        }
        return page([], 1);
      },
    );
    const port: ChatCorePort = {
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
    const { result, rerender } = renderHook(
      ({ liveEvents }: { readonly liveEvents: readonly RuntimeEvent[] }) =>
        useSubagentFeed(port, "chat.subagents", "child.a", 10, liveEvents, true),
      { initialProps: { liveEvents: live } },
    );
    expect(result.current.events.map((item) => item.sequence)).toEqual([8, 9]);
    expect(result.current.hasOlder).toBe(true);
    await act(async () => {
      await result.current.loadOlder();
    });
    expect(subagentEvents).toHaveBeenCalledTimes(2);
    expect(result.current.events.map((item) => item.sequence)).toEqual([
      3, 5, 8, 9,
    ]);
    expect(result.current.firstSequence).toBe(3);
    // History is only exhausted once a page actually reaches sequence 1.
    expect(result.current.hasOlder).toBe(true);
    await act(async () => {
      await result.current.loadOlder();
    });
    expect(subagentEvents).toHaveBeenCalledTimes(3);
    expect(result.current.hasOlder).toBe(false);
    // Live delivery extends the same scope without refetching.
    rerender({ liveEvents: [...live, childFact(10, "child.a")] });
    expect(result.current.events.map((item) => item.sequence)).toEqual([
      3, 5, 8, 9, 10,
    ]);
    expect(subagentEvents).toHaveBeenCalledTimes(3);
  });

  it("never pages while the child tab is inactive", async () => {
    const subagentEvents = vi.fn();
    const port: ChatCorePort = {
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
    const { result } = renderHook(() =>
      useSubagentFeed(port, "chat.subagents", "child.a", 10, [], false),
    );
    expect(result.current.hasOlder).toBe(false);
    await act(async () => {
      await result.current.loadOlder();
    });
    expect(subagentEvents).not.toHaveBeenCalled();
  });
});
