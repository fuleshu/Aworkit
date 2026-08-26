import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ChatIntent, LiveChatActivity } from "./types";
import { reduceRunEventProjection } from "./activityProjection";
import {
  createChatCorePort,
  type ChatCorePort,
  type RuntimeEvent,
  type RuntimeSnapshot,
} from "./corePort";

export interface ChatRuntimeState {
  readonly snapshot: RuntimeSnapshot | null;
  readonly events: readonly RuntimeEvent[];
  readonly liveActivities: readonly LiveChatActivity[];
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
  const [liveActivities, setLiveActivities] = useState<
    readonly LiveChatActivity[]
  >([]);
  const liveActivitiesRef = useRef<readonly LiveChatActivity[]>([]);
  const pendingRef = useRef<Set<string>>(new Set());
  const pendingRunIdsRef = useRef<Map<string, string>>(new Map());
  const activityReadyRef = useRef<Promise<void>>(Promise.resolve());
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

  const projectLiveActivity = useCallback(
    (activity: LiveChatActivity): void => {
      try {
        const next = reduceRunEventProjection(
          liveActivitiesRef.current,
          activity,
        );
        liveActivitiesRef.current = next;
        setLiveActivities(next);
      } catch (failure) {
        setStale(true);
        setError(message(failure));
        void resynchronize();
      }
    },
    [resynchronize],
  );

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
    let current = true;
    let timer: number | undefined;
    const poll = async () => {
      await refresh();
      if (current) timer = window.setTimeout(() => void poll(), pollIntervalMs);
    };
    // Schedule the next read only after the preceding read settles. During a
    // long Run the native snapshot may legitimately wait for runtime ownership;
    // single-flight polling prevents a queue of stale reads from racing the
    // canonical terminal projection afterward.
    void poll();
    return () => {
      current = false;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [pollIntervalMs, refresh]);

  useEffect(() => {
    if (port.subscribeActivity === undefined) return;
    let dispose: (() => void) | undefined;
    let current = true;
    const ready = port
      .subscribeActivity((activity) => {
        const belongsToPendingRun = [...pendingRunIdsRef.current.values()].some(
          (runId) => runId === activity.runId,
        );
        if (
          !pendingRef.current.has(activity.requestId) &&
          !belongsToPendingRun
        )
          return;
        projectLiveActivity(activity);
      })
      .then((unsubscribe) => {
        if (current) dispose = unsubscribe;
        else unsubscribe();
      })
      .catch(() => {
        // Polling and the immediate local busy card remain available if native
        // transient event delivery is unsupported.
      });
    activityReadyRef.current = ready;
    return () => {
      current = false;
      dispose?.();
      if (activityReadyRef.current === ready)
        activityReadyRef.current = Promise.resolve();
    };
  }, [port, projectLiveActivity]);

  const dispatch = useCallback(
    async (intent: ChatIntent): Promise<boolean> => {
      if (snapshot === null || stale) return false;
      pendingRef.current.add(intent.commandId);
      pendingRunIdsRef.current.set(intent.commandId, snapshot.chat.runId);
      setPending((current) => new Set([...current, intent.commandId]));
      projectLiveActivity({
          requestId: intent.commandId,
          runId: snapshot.chat.runId,
          activityId: `busy.${intent.commandId}`,
          kind: "thinking",
          title: "Thinking",
          body: "Aworkit is working…",
          status: "running",
      });
      try {
        // A fast local provider can emit its first chunks synchronously with
        // command admission. Do not start it until the native event listener is
        // confirmed, otherwise those first states are permanently lost.
        await activityReadyRef.current;
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
        const pendingRunId = pendingRunIdsRef.current.get(intent.commandId);
        pendingRef.current.delete(intent.commandId);
        pendingRunIdsRef.current.delete(intent.commandId);
        const nextLiveActivities = liveActivitiesRef.current.filter(
          (activity) =>
            activity.requestId !== intent.commandId &&
            activity.runId !== pendingRunId,
        );
        liveActivitiesRef.current = nextLiveActivities;
        setLiveActivities(nextLiveActivities);
        setPending((current) => {
          const next = new Set(current);
          next.delete(intent.commandId);
          return next;
        });
      }
    },
    [port, projectLiveActivity, resynchronize, snapshot, stale],
  );

  return {
    snapshot,
    events,
    liveActivities,
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
