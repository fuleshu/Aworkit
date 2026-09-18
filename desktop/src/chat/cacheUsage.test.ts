import { describe, expect, it } from "vitest";
import { cacheUsageFields } from "./cacheUsage";
import type { RuntimeEvent } from "./corePort";

function event(id: string, cache?: unknown, kind = "span.usage"): RuntimeEvent {
  return { eventId: id, kind, payload: { spanId: id, callId: id, inputTokens: 100, cache } } as RuntimeEvent;
}
const values = (events: RuntimeEvent[], reviews = false) => Object.fromEntries(cacheUsageFields(events, reviews).map(f => [f.label, f.value]));
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
