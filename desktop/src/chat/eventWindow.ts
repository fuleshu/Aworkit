import type { RuntimeEvent } from "./corePort";

export function assertSameEnvelope(left: RuntimeEvent, right: RuntimeEvent): void {
  if (left !== right && JSON.stringify(left) !== JSON.stringify(right)) throw new Error(`canonical event conflict at sequence ${left.sequence}`);
}
export function assertContiguous(events: readonly RuntimeEvent[], first = 1): void {
  for (let index = 0; index < events.length; index++) {
    if (events[index].sequence !== first + index) throw new Error(`projection gap: expected sequence ${first + index}, received ${events[index].sequence}`);
  }
}
export function mergeCanonicalEvents(current: readonly RuntimeEvent[], incoming: readonly RuntimeEvent[], first = 1): RuntimeEvent[] {
  const bySequence = new Map<number, RuntimeEvent>();
  for (const event of [...current, ...incoming]) {
    const existing = bySequence.get(event.sequence);
    if (existing) assertSameEnvelope(existing, event); else bySequence.set(event.sequence, event);
  }
  const merged = [...bySequence.values()].sort((a,b) => a.sequence - b.sequence);
  assertContiguous(merged, first);
  return merged;
}
/** Support is not a contiguous prefix. It only supplies exact span facts to
 * projection; canonical cursor validation always uses the window separately. */
export function withEventSupport(events: readonly RuntimeEvent[], support: readonly RuntimeEvent[]): RuntimeEvent[] {
  const bySequence = new Map<number, RuntimeEvent>();
  for (const event of [...support, ...events]) {
    const previous = bySequence.get(event.sequence);
    if (previous) assertSameEnvelope(previous, event);
    bySequence.set(event.sequence, event);
  }
  return [...bySequence.values()].sort((a,b) => a.sequence - b.sequence);
}
