import { describe, expect, it } from "vitest";
import { cacheUsageFields, reportedCacheFields, reportedCacheUnits } from "./cacheUsage";
import type { RuntimeEvent } from "./corePort";

function event(id: string, cache?: unknown, kind = "span.usage"): RuntimeEvent {
  return { eventId: id, kind, payload: { spanId: id, callId: id, inputTokens: 100, cache } } as RuntimeEvent;
}
const values = (events: RuntimeEvent[], reviews = false, scoped = false) =>
  Object.fromEntries(cacheUsageFields(events, reviews, scoped).map(f => [f.label, f.value]));
describe("provider cache usage", () => {
  it("keeps unknown distinct from a reported zero and limits the ratio denominator", () => {
    expect(values([event("old")])["Cached input tokens"]).toBe("Not reported");
    const v = values([event("old"), event("new", { cachedInputTokens: 0, cacheMissInputTokens: 100 })]);
    expect(v["Cached input tokens"]).toBe("0 (1/2 calls reported)");
    expect(v["Cache hit rate"]).toBe("0.0% of reported calls");
  });
  it("counts only per-request facts and separates reviews", () => {
    const cache = { cachedInputTokens: 80, cacheMissInputTokens: 20 };
    const events = [event("model", cache), event("review", cache, "approval.reviewed"), event("snapshot", cache, "context.checkpoint")];
    expect(values(events)["Cache hit rate"]).toBe("80.0%");
    expect(values(events, true)["Cached input tokens"]).toBe("80");
    expect(values([...events, events[0]!])["Cached input tokens"]).toBe("80");
  });
});
describe("whole-run cache usage", () => {
  it("reads the aggregate a Run summary reported and never a window", () => {
    const summary = {
      eventId: "summary", kind: "message.assistant",
      payload: { inputUnits: 7_271_744, outputUnits: 188_758, cachedInputUnits: 7_207_552, uncachedInputUnits: 64_192 },
    } as RuntimeEvent;
    const window = [summary, event("turn", { cachedInputTokens: 347_008, cacheMissInputTokens: 2_697 })];
    expect(reportedCacheUnits(window)).toEqual({ cached: 7_207_552, uncached: 64_192 });
    expect(Object.fromEntries(reportedCacheFields(reportedCacheUnits(window)!).map(f => [f.label, f.value]))).toEqual({
      "Cached input tokens": "7,207,552",
      "Uncached input tokens": "64,192",
      "Cache hit rate": "99.1%",
    });
  });
  it("marks a windowed subset as loaded activity and never as a Run total", () => {
    const v = values([event("turn", { cachedInputTokens: 80, cacheMissInputTokens: 20 })], false, true);
    expect(v["Cached input tokens in loaded activity"]).toBe("80 (1 call loaded)");
    expect(v["Uncached input tokens in loaded activity"]).toBe("20 (1 call loaded)");
    expect(v["Cache hit rate"]).toBe("80.0% of loaded activity");
  });
});
