import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ChatIntent } from "./types";
import {
  createChatCorePort,
  type ChatCorePort,
  type RuntimeEvent,
  type RuntimeSnapshot,
} from "./corePort";

export interface ChatRuntimeState {
  readonly snapshot: RuntimeSnapshot | null;
  readonly events: readonly RuntimeEvent[];
  readonly stale: boolean;
  readonly loading: boolean;
  readonly error: string | null;
  readonly pendingCommandIds: ReadonlySet<string>;
  dispatch(intent: ChatIntent): Promise<boolean>;
  resynchronize(): Promise<boolean>;
}

/**
 * Maintains one contiguous projection of the canonical committed event stream.
 * Live notifications and snapshots carry the same envelopes; neither source
 * owns a second reducer or a replace-at-settlement representation.
 */
export function useChatRuntime(
  explicitPort?: ChatCorePort,
  pollIntervalMs = 2_000,
): ChatRuntimeState {
  const port = useMemo(
    () => explicitPort ?? createChatCorePort(),
    [explicitPort],
  );
  const [snapshot, setSnapshot] = useState<RuntimeSnapshot | null>(null);
  const snapshotRef = useRef<RuntimeSnapshot | null>(null);
  const eventsRef = useRef<RuntimeEvent[]>([]);
  const bufferedRef = useRef<Map<number, RuntimeEvent>>(new Map());
  const initializedRef = useRef(false);
  const [events, setEvents] = useState<readonly RuntimeEvent[]>([]);
  const pendingRef = useRef<Set<string>>(new Set());
  const eventReadyRef = useRef<Promise<void>>(Promise.resolve());
  const [stale, setStale] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [pendingCommandIds, setPending] = useState<ReadonlySet<string>>(
    new Set(),
  );

  const failProjection = useCallback((failure: unknown): void => {
    setStale(true);
    setError(message(failure));
  }, []);

  const publishEvents = useCallback((next: RuntimeEvent[]): void => {
    eventsRef.current = next;
    setEvents(next);
  }, []);

  const ingestLiveEvent = useCallback(
    (event: RuntimeEvent): void => {
      try {
        if (!initializedRef.current) {
          bufferEvent(bufferedRef.current, event);
          return;
        }
        const existing = eventsRef.current[event.sequence - 1];
        if (existing !== undefined) {
          assertSameEnvelope(existing, event);
          return;
        }
        bufferEvent(bufferedRef.current, event);
        const next = [...eventsRef.current];
        drainContiguous(next, bufferedRef.current);
        if (next.length !== eventsRef.current.length) publishEvents(next);
      } catch (failure) {
        failProjection(failure);
      }
    },
    [failProjection, publishEvents],
  );

  const replaceSnapshot = useCallback(
    (next: RuntimeSnapshot, full: boolean): void => {
      const merged = mergeCanonicalEvents(
        full ? [] : eventsRef.current,
        next.events,
      );
      if (full && merged.length < next.throughSequence) {
        throw new Error(
          `projection gap: snapshot ended at ${merged.length}, head is ${next.throughSequence}`,
        );
      }
      const buffered = [...bufferedRef.current.values()].sort(
        (left, right) => left.sequence - right.sequence,
      );
      for (const event of buffered) mergeOne(merged, event);
      bufferedRef.current.clear();
      assertContiguous(merged);
      initializedRef.current = true;
      snapshotRef.current = next;
      setSnapshot(next);
      publishEvents(merged);
    },
    [publishEvents],
  );

  const resynchronize = useCallback(async (): Promise<boolean> => {
    try {
      const next = await port.snapshot(0);
      if (
        snapshotRef.current !== null &&
        next.throughSequence < snapshotRef.current.throughSequence
      ) {
        throw new Error(
          "trusted-core snapshot moved behind the last contiguous projection",
        );
      }
      replaceSnapshot(next, true);
      setStale(false);
      setError(null);
      return true;
    } catch (failure) {
      failProjection(failure);
      return false;
    } finally {
      setLoading(false);
    }
  }, [failProjection, port, replaceSnapshot]);

  const refresh = useCallback(async (): Promise<void> => {
    // A running command is driven exclusively by pushed committed events. The
    // desktop runtime owns the command during execution, so polling here would
    // only queue a blocked snapshot call behind it.
    if (pendingRef.current.size > 0) return;
    const current = snapshotRef.current;
    if (current === null) {
      await resynchronize();
      return;
    }
    try {
      const next = await port.snapshot(current.throughSequence);
      if (next.throughSequence < current.throughSequence) {
        throw new Error("trusted-core snapshot moved backwards");
      }
      replaceSnapshot(next, false);
      setStale(false);
      setError(null);
    } catch (failure) {
      failProjection(failure);
    }
  }, [failProjection, port, replaceSnapshot, resynchronize]);

  // Register the push listener before the initial snapshot. Events committed
  // during that race are buffered by sequence and deduplicated against replay.
  useEffect(() => {
    if (port.subscribeEvents === undefined) return;
    let dispose: (() => void) | undefined;
    let current = true;
    const ready = port
      .subscribeEvents(ingestLiveEvent)
      .then((unsubscribe) => {
        if (current) dispose = unsubscribe;
        else unsubscribe();
      })
      .catch((failure) => {
        failProjection(failure);
      });
    eventReadyRef.current = ready;
    return () => {
      current = false;
      dispose?.();
      if (eventReadyRef.current === ready)
        eventReadyRef.current = Promise.resolve();
    };
  }, [failProjection, ingestLiveEvent, port]);

  useEffect(() => {
    let current = true;
    let timer: number | undefined;
    const poll = async () => {
      await refresh();
      if (current) timer = window.setTimeout(() => void poll(), pollIntervalMs);
    };
    void poll();
    return () => {
      current = false;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [pollIntervalMs, refresh]);

  const dispatch = useCallback(
    async (intent: ChatIntent): Promise<boolean> => {
      const current = snapshotRef.current;
      if (current === null || stale) return false;
      pendingRef.current.add(intent.commandId);
      setPending((value) => new Set([...value, intent.commandId]));
      try {
        await eventReadyRef.current;
        const receipt = await port.command(intent, current.version);
        if (!receipt.accepted) {
          const reason =
            receipt.reason ?? "The trusted core rejected the command.";
          await resynchronize();
          setError(reason);
          return false;
        }
        return await resynchronize();
      } catch (failure) {
        const failureMessage = message(failure);
        await resynchronize();
        setError(failureMessage);
        return false;
      } finally {
        pendingRef.current.delete(intent.commandId);
        setPending((value) => {
          const next = new Set(value);
          next.delete(intent.commandId);
          return next;
        });
      }
    },
    [port, resynchronize, stale],
  );

  return {
    snapshot,
    events,
    stale,
    loading,
    error,
    pendingCommandIds,
    dispatch,
    resynchronize,
  };
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function bufferEvent(
  buffered: Map<number, RuntimeEvent>,
  event: RuntimeEvent,
): void {
  const existing = buffered.get(event.sequence);
  if (existing !== undefined) assertSameEnvelope(existing, event);
  else buffered.set(event.sequence, event);
}

function drainContiguous(
  events: RuntimeEvent[],
  buffered: Map<number, RuntimeEvent>,
): void {
  for (;;) {
    const sequence = events.length + 1;
    const event = buffered.get(sequence);
    if (event === undefined) return;
    buffered.delete(sequence);
    events.push(event);
  }
}

function mergeCanonicalEvents(
  current: readonly RuntimeEvent[],
  incoming: readonly RuntimeEvent[],
): RuntimeEvent[] {
  const merged = [...current];
  for (const event of incoming) mergeOne(merged, event);
  merged.sort((left, right) => left.sequence - right.sequence);
  assertContiguous(merged);
  return merged;
}

function mergeOne(base: RuntimeEvent[], event: RuntimeEvent): void {
  const existing = base.find((candidate) => candidate.sequence === event.sequence);
  if (existing !== undefined) {
    assertSameEnvelope(existing, event);
    return;
  }
  base.push(event);
}

function assertContiguous(events: readonly RuntimeEvent[]): void {
  const sorted = [...events].sort((left, right) => left.sequence - right.sequence);
  for (let index = 0; index < sorted.length; index += 1) {
    const expected = index + 1;
    if (sorted[index]?.sequence !== expected) {
      throw new Error(
        `projection gap: expected sequence ${expected}, received ${sorted[index]?.sequence ?? "end"}`,
      );
    }
  }
}

function assertSameEnvelope(
  left: RuntimeEvent,
  right: RuntimeEvent,
): void {
  if (JSON.stringify(left) !== JSON.stringify(right)) {
    throw new Error(`canonical event conflict at sequence ${left.sequence}`);
  }
}
