// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { ChatCorePort, ChatEventPage, RuntimeEvent, RuntimeSnapshot } from "./corePort";
import { chatSnapshot } from "../test/fixtures/chat";

/** Sizes of the event arrays handed to the semantic projection. */
const projectedSizes = vi.hoisted(() => [] as number[]);
vi.mock("./activityProjection", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./activityProjection")>();
  return {
    ...actual,
    projectSemanticTimeline: (events: readonly RuntimeEvent[]) => {
      projectedSizes.push(events.length);
      return actual.projectSemanticTimeline(events);
    },
  };
});

import { useChatRuntime } from "./useChatRuntime";
import { filterTimelineItems } from "./chatOutputFilter";
import type { TimelineItem } from "./types";

afterEach(cleanup);

it("projects each older page on its own instead of the whole loaded range", async () => {
  const pageSize = 10;
  const events = (id: string, first: number, last: number): RuntimeEvent[] =>
    Array.from({ length: last - first + 1 }, (_, n) => ({
      schemaVersion: 1, streamId: id, branchId: "main", sequence: first + n,
      eventId: `${id}.${first + n}`, kind: "message.user", payload: { body: String(first + n) },
    }));
  const next: RuntimeSnapshot = chatSnapshot({
    chat: { chatId: "a", runId: "a", title: "a" },
    history: [],
    events: events("a", 101, 110),
    eventWindow: { firstSequence: 101, lastSequence: 110, headSequence: 110, hasMore: true, supportingEvents: [] },
  });
  const port: ChatCorePort = {
    async snapshot() { return next; },
    async command() { throw Error("unused"); },
    // Each page ends with one shown message and is otherwise hidden tool facts,
    // so a page reveals exactly one entry once tools are hidden.
    async olderEvents(_id, before): Promise<ChatEventPage> {
      const first = before - pageSize;
      const page = events("a", first, before - 1).map(event =>
        event.sequence === before - 1
          ? event
          : { ...event, kind: "tool.completed", payload: { capabilityId: "tool.files.list" } });
      return { events: page, window: { firstSequence: first, lastSequence: before - 1, headSequence: 110, hasMore: true, supportingEvents: [] } };
    },
  };
  const hideTools = (items: readonly TimelineItem[]) =>
    filterTimelineItems(items, { thinking: true, tools: false });
  const { result } = renderHook(() => useChatRuntime(port, 60_000, hideTools, 5));
  await waitFor(() => expect(result.current.firstSequence).toBe(101));

  projectedSizes.length = 0;
  await act(async () => { await result.current.loadOlder(); });

  // One projection per page plus the baseline. If the loader re-merged and
  // re-projected the whole accumulated range each page (the old behaviour),
  // the largest array would grow with the page count instead of staying a page.
  expect(result.current.firstSequence).toBe(51);
  expect(projectedSizes.length).toBeLessThanOrEqual(6);
  expect(Math.max(...projectedSizes)).toBeLessThanOrEqual(pageSize);
});
