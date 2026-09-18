/** Project optional provider evidence without converting missing usage to zero.
 * Only canonical per-request usage events count; embedded history never does. */
import type { RuntimeEvent } from "./corePort";
import type { RunDetailField } from "./runDetails";

type RecordValue = Record<string, unknown>;
function record(value: unknown): RecordValue {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as RecordValue : {};
}
function count(value: unknown): number | undefined {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : undefined;
}

export function cacheUsageFields(events: readonly RuntimeEvent[], reviews = false): RunDetailField[] {
  const requests = new Map<string, RecordValue>();
  for (const event of events) {
    const p = record(event.payload);
    if (reviews ? event.kind !== "approval.reviewed"
      : event.kind !== "span.usage" && event.kind !== "context.compaction-ended") continue;
    const key = reviews ? event.eventId
      : String(p.spanId ?? event.eventId);
    requests.set(key, event.kind === "context.compaction-ended" ? record(p.auxiliary) : p);
  }
  if (requests.size === 0) return [];
  let hits = 0, misses = 0, hitReports = 0, missReports = 0, pairedHits = 0, pairedInput = 0;
  for (const p of requests.values()) {
    const cache = record(p.cache);
    const hit = count(cache.cachedInputTokens), miss = count(cache.cacheMissInputTokens);
    if (hit !== undefined) { hits += hit; hitReports++; }
    if (miss !== undefined) { misses += miss; missReports++; }
    const input = count(p.inputTokens);
    // The denominator belongs to exactly the calls that reported hits.
    if (hit !== undefined && input !== undefined && hit <= input) {
      pairedHits += hit; pairedInput += input;
    }
  }
  const value = (total: number, reports: number) => reports === 0 ? "Not reported"
    : `${total.toLocaleString()}${reports < requests.size ? ` (${reports}/${requests.size} calls reported)` : ""}`;
  return [
    { label: "Cached input tokens", value: value(hits, hitReports) },
    { label: "Uncached input tokens", value: value(misses, missReports) },
    { label: "Cache hit rate", value: pairedInput === 0 ? "Not available"
      : `${(100 * pairedHits / pairedInput).toFixed(1)}%${hitReports < requests.size ? " of reported calls" : ""}` },
  ];
}
