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

/** Maintains a contiguous, immutable projection over the native trusted-core port. */
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
  const [events, setEvents] = useState<readonly RuntimeEvent[]>([]);
  const [stale, setStale] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [pendingCommandIds, setPending] = useState<ReadonlySet<string>>(
    new Set(),
  );

  const replaceSnapshot = useCallback((next: RuntimeSnapshot) => {
    snapshotRef.current = next;
    setSnapshot(next);
    eventsRef.current = mergeEvents(eventsRef.current, next.events);
    setEvents(eventsRef.current);
  }, []);

  const resynchronize = useCallback(async (): Promise<boolean> => {
    try {
      const next = await port.snapshot(0);
      if (
        snapshotRef.current !== null &&
        next.lastSequence < snapshotRef.current.lastSequence
      ) {
        throw new Error(
          "trusted-core snapshot moved behind the last contiguous projection",
        );
      }
      replaceSnapshot(next);
      setStale(false);
      setError(null);
      return true;
    } catch (failure) {
      setStale(true);
      setError(message(failure));
      return false;
    } finally {
      setLoading(false);
    }
  }, [port, replaceSnapshot]);

  const refresh = useCallback(async (): Promise<void> => {
    const current = snapshotRef.current;
    if (current === null) {
      await resynchronize();
      return;
    }
    try {
      const next = await port.snapshot(current.lastSequence);
      if (next.lastSequence === current.lastSequence) {
        if (!sameProjectedSnapshot(current, next)) replaceSnapshot(next);
        return;
      }
      let expected = current.lastSequence + 1;
      for (const event of next.events) {
        if (event.sequence !== expected) {
          throw new Error(
            `projection gap: expected sequence ${expected}, received ${event.sequence}`,
          );
        }
        expected += 1;
      }
      if (expected - 1 !== next.lastSequence) {
        throw new Error(
          `projection gap: delta ended at ${expected - 1}, snapshot is ${next.lastSequence}`,
        );
      }
      replaceSnapshot(next);
      setStale(false);
      setError(null);
    } catch (failure) {
      setStale(true);
      setError(message(failure));
    }
  }, [port, replaceSnapshot, resynchronize]);

  useEffect(() => {
    void resynchronize();
    const timer = window.setInterval(() => {
      void refresh();
    }, pollIntervalMs);
    return () => window.clearInterval(timer);
  }, [pollIntervalMs, refresh, resynchronize]);

  const dispatch = useCallback(
    async (intent: ChatIntent): Promise<boolean> => {
      if (snapshot === null || stale) return false;
      setPending((current) => new Set([...current, intent.commandId]));
      try {
        const receipt = await port.command(intent, snapshot.version);
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
        setPending((current) => {
          const next = new Set(current);
          next.delete(intent.commandId);
          return next;
        });
      }
    },
    [port, resynchronize, snapshot, stale],
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

/** Merges ordered event deltas, deduplicating by sequence for idempotent refresh. */
function mergeEvents(
  current: readonly RuntimeEvent[],
  incoming: readonly RuntimeEvent[],
): RuntimeEvent[] {
  if (incoming.length === 0) return current as RuntimeEvent[];
  const bySequence = new Map<number, RuntimeEvent>();
  for (const event of current) bySequence.set(event.sequence, event);
  for (const event of incoming) bySequence.set(event.sequence, event);
  return [...bySequence.values()].sort(
    (left, right) => left.sequence - right.sequence,
  );
}

/** Session recovery and other auxiliary projections can change durably before
 * the semantic history head advances, so lastSequence alone is not freshness. */
function sameProjectedSnapshot(
  current: RuntimeSnapshot,
  next: RuntimeSnapshot,
): boolean {
  return (
    current.version === next.version &&
    JSON.stringify(current.chat) === JSON.stringify(next.chat) &&
    JSON.stringify(current.projects) === JSON.stringify(next.projects) &&
    JSON.stringify(current.timeline) === JSON.stringify(next.timeline) &&
    JSON.stringify(current.evidence) === JSON.stringify(next.evidence)
  );
}
