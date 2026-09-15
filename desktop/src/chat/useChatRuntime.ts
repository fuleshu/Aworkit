import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ChatIntent } from "./types";
import { useChatMaintenance } from "./useChatMaintenance";
import {
  createChatCorePort,
  type ChatCorePort,
  type RuntimeEvent,
  type RuntimeSnapshot,
} from "./corePort";

export interface ChatRuntimeState {
  readonly contextModel?: ChatCorePort["contextModel"];
  readonly snapshot: RuntimeSnapshot | null;
  readonly events: readonly RuntimeEvent[];
  readonly stale: boolean;
  readonly loading: boolean;
  readonly error: RuntimeErrorNotice | null;
  readonly pendingCommandIds: ReadonlySet<string>;
  readonly maintenancePending: boolean;
  readonly queuedMaintenanceInputs: readonly string[];
  dispatch(intent: ChatIntent, expectedVersion?: number): Promise<boolean>;
  resynchronize(): Promise<boolean>;
  dismissError(): void;
}

export interface RuntimeErrorNotice {
  readonly id: number;
  readonly message: string;
}

/**
 * Window for one coalesced publish of streamed content. Provider chunk rate
 * never reaches React: a burst of `span.content_delta` envelopes produces a
 * single state update, while every lifecycle fact still publishes in the call
 * that ingested it.
 */
const STREAM_PUBLISH_INTERVAL_MS = 50;

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
  const contextModel = useMemo(() => port.contextModel?.bind(port), [port]);
  const [snapshot, setSnapshot] = useState<RuntimeSnapshot | null>(null);
  const snapshotRef = useRef<RuntimeSnapshot | null>(null);
  const eventsRef = useRef<RuntimeEvent[]>([]);
  const bufferedRef = useRef<Map<string, RuntimeEvent>>(new Map());
  const initializedRef = useRef(false);
  const [events, setEvents] = useState<readonly RuntimeEvent[]>([]);
  const flushTimerRef = useRef<number | undefined>(undefined);
  const pendingRef = useRef(new Map<string, string>());
  const generationRef = useRef(0);
  const snapshotRequestsRef = useRef({ requested: 0, applied: 0 });
  const navigatingRef = useRef(false);
  const navigationTail = useRef(Promise.resolve());
  const errorsRef = useRef(new Map<string, RuntimeErrorNotice>());
  const eventReadyRef = useRef<Promise<void>>(Promise.resolve());
  const nextErrorIdRef = useRef(0);
  const lastFailureRef = useRef<string | null>(null);
  const [stale, setStale] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<RuntimeErrorNotice | null>(null);
  const [allPendingCommandIds, setPending] = useState<ReadonlySet<string>>(
    new Set(),
  );

  const reportError = useCallback((failure: unknown): void => {
    const failureMessage = message(failure);
    // A disconnected poll can repeat the same failure indefinitely. Keep one
    // acknowledged occurrence until a healthy core call resets the cycle.
    if (lastFailureRef.current === failureMessage) return;
    lastFailureRef.current = failureMessage;
    nextErrorIdRef.current += 1;
    setError({ id: nextErrorIdRef.current, message: failureMessage });
  }, []);

  const markHealthy = useCallback((): void => {
    lastFailureRef.current = null;
  }, []);

  const dismissError = useCallback((): void => {
    if (snapshotRef.current) errorsRef.current.delete(snapshotRef.current.chat.chatId);
    setError(null);
  }, []);

  const failProjection = useCallback(
    (failure: unknown): void => {
      setStale(true);
      reportError(failure);
    },
    [reportError],
  );

  /** Drops a scheduled coalesced publish; the authoritative projection is untouched. */
  const cancelDeferredPublish = useCallback((): void => {
    if (flushTimerRef.current === undefined) return;
    window.clearTimeout(flushTimerRef.current);
    flushTimerRef.current = undefined;
  }, []);

  /**
   * Publishes the contiguous projection immediately. Every publish supersedes
   * a pending deferred delta flush: `eventsRef.current` already carries the
   * drained deltas, so a second flush would only re-render identical content
   * from a stale array reference. The rendered value is an independent copy so
   * the authoritative projection can keep growing in place between publishes.
   */
  const publishEvents = useCallback(
    (next: RuntimeEvent[]): void => {
      cancelDeferredPublish();
      eventsRef.current = next;
      setEvents(next.slice());
    },
    [cancelDeferredPublish],
  );

  /**
   * Coalesces streamed content into one React publish per window. The
   * authoritative projection in `eventsRef` still grows with every drained
   * event; only the visible projection is rate-limited, so no delta is lost,
   * duplicated, or reordered.
   */
  const scheduleDeferredPublish = useCallback((): void => {
    if (flushTimerRef.current !== undefined) return;
    flushTimerRef.current = window.setTimeout(() => {
      flushTimerRef.current = undefined;
      setEvents(eventsRef.current.slice());
    }, STREAM_PUBLISH_INTERVAL_MS);
  }, []);

  useEffect(() => cancelDeferredPublish, [cancelDeferredPublish]);

  const ingestLiveEvent = useCallback(
    (event: RuntimeEvent): void => {
      try {
        if (!initializedRef.current) {
          bufferEvent(bufferedRef.current, event);
          return;
        }
        // The native subscription is process-wide. A delayed envelope from a
        // previously selected Chat is valid history, but it does not belong in
        // the active stream projection and must not collide by sequence.
        if (event.streamId !== snapshotRef.current?.chat.chatId) return;
        const existing = eventsRef.current[event.sequence - 1];
        if (existing !== undefined) {
          assertSameEnvelope(existing, event);
          return;
        }
        bufferEvent(bufferedRef.current, event);
        const head = eventsRef.current.length;
        drainContiguous(eventsRef.current, bufferedRef.current, event.streamId);
        if (eventsRef.current.length === head) return;
        // Only streamed content may be deferred. A drained batch that carries
        // any lifecycle fact publishes in this same call, because synchronous
        // callers and lifecycle ordering in the UI depend on it.
        if (onlyStreamedContent(eventsRef.current, head)) {
          scheduleDeferredPublish();
          return;
        }
        publishEvents(eventsRef.current);
      } catch (failure) {
        failProjection(failure);
      }
    },
    [failProjection, publishEvents, scheduleDeferredPublish],
  );

  const replaceSnapshot = useCallback(
    (next: RuntimeSnapshot, full: boolean): void => {
      if (next.events.some((event) => event.streamId !== next.chat.chatId)) {
        throw new Error("trusted-core snapshot contains a foreign Chat stream");
      }
      const merged = mergeCanonicalEvents(
        full && next.chat.chatId !== snapshotRef.current?.chat.chatId ? [] : eventsRef.current,
        next.events,
      );
      if (full && merged.length < next.throughSequence) {
        throw new Error(
          `projection gap: snapshot ended at ${merged.length}, head is ${next.throughSequence}`,
        );
      }
      const buffered = [...bufferedRef.current.values()].sort(
        (left, right) => left.sequence - right.sequence,
      ).filter((event) => event.streamId === next.chat.chatId);
      for (const event of buffered) mergeOne(merged, event);
      bufferedRef.current.clear();
      assertContiguous(merged);
      initializedRef.current = true;
      snapshotRef.current = next;
      setError(errorsRef.current.get(next.chat.chatId) ?? null);
      setSnapshot(next);
      publishEvents(merged);
    },
    [publishEvents],
  );

  const resynchronize = useCallback(async (replaceStream = false): Promise<boolean> => {
    const generation = generationRef.current;
    const request = ++snapshotRequestsRef.current.requested;
    const chatId = replaceStream ? undefined : snapshotRef.current?.chat.chatId;
    try {
      const next = await port.snapshot(0, chatId);
      if (generation !== generationRef.current || request < snapshotRequestsRef.current.applied || (!replaceStream && navigatingRef.current)) return true;
      if (chatId !== undefined && next.chat.chatId !== chatId) return true;
      if (
        !replaceStream &&
        snapshotRef.current !== null &&
        next.throughSequence < snapshotRef.current.throughSequence
      ) {
        throw new Error(
          "trusted-core snapshot moved behind the last contiguous projection",
        );
      }
      replaceSnapshot(next, true);
      snapshotRequestsRef.current.applied = request;
      setStale(false);
      markHealthy();
      return true;
    } catch (failure) {
      if (generation !== generationRef.current || request < snapshotRequestsRef.current.applied) return true;
      failProjection(failure);
      return false;
    } finally {
      setLoading(false);
    }
  }, [failProjection, markHealthy, port, replaceSnapshot]);

  const refresh = useCallback(async (): Promise<void> => {
    if (navigatingRef.current) return;
    const current = snapshotRef.current;
    const generation = generationRef.current;
    if (current === null) {
      await resynchronize();
      return;
    }
    const request = ++snapshotRequestsRef.current.requested;
    try {
      const next = await port.snapshot(current.throughSequence, current.chat.chatId);
      if (generation !== generationRef.current || request < snapshotRequestsRef.current.applied || navigatingRef.current || next.chat.chatId !== current.chat.chatId) return;
      if (next.throughSequence < current.throughSequence) {
        throw new Error("trusted-core snapshot moved backwards");
      }
      replaceSnapshot(next, false);
      snapshotRequestsRef.current.applied = request;
      setStale(false);
      markHealthy();
    } catch (failure) {
      if (generation !== generationRef.current || request < snapshotRequestsRef.current.applied) return;
      failProjection(failure);
    }
  }, [failProjection, markHealthy, port, replaceSnapshot, resynchronize]);

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

  const execute = useCallback(
    async (intent: ChatIntent, expectedVersion?: number): Promise<boolean> => {
      const current = snapshotRef.current;
      if (current === null || stale) return false;
      const navigation = replacesSelectedChat(intent);
      const chatId = intent.targetId ?? current.chat.chatId;
      pendingRef.current.set(intent.commandId, chatId);
      setPending((value) => new Set([...value, intent.commandId]));
      if (navigation) { generationRef.current += 1; navigatingRef.current = true; }
      const commandError = (failure: unknown) => {
        const notice = { id: ++nextErrorIdRef.current, message: message(failure) };
        errorsRef.current.set(chatId, notice);
        if (snapshotRef.current?.chat.chatId === chatId) setError(notice);
      };
      try {
        await eventReadyRef.current;
        const version = expectedVersion ?? (chatId === current.chat.chatId ? current.version : (await port.snapshot(0, chatId)).version);
        const receipt = await port.command(intent, version);
        if (!receipt.accepted) {
          const reason =
            receipt.reason ?? "The trusted core rejected the command.";
          await resynchronize(replacesSelectedChat(intent));
          commandError(reason);
          return false;
        }
        errorsRef.current.delete(chatId);
        await resynchronize(replacesSelectedChat(intent));
        return true;
      } catch (failure) {
        const failureMessage = message(failure);
        // The command response can be lost after a stream-changing mutation
        // committed. Recovery must accept the newly selected stream even when
        // its sequence is lower than the previously visible Chat.
        const recovered = await resynchronize(replacesSelectedChat(intent));
        if (
          recovered &&
          intent.type === "select_chat" &&
          snapshotRef.current?.chat.chatId === intent.targetId
        )
          return true;
        commandError(failureMessage);
        return false;
      } finally {
        if (navigation) navigatingRef.current = false;
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

  const maintenance = useChatMaintenance(execute);
  const dispatch = useCallback((intent: ChatIntent, version?: number): Promise<boolean> => {
    const captured = { ...intent, targetId: intent.targetId ?? snapshotRef.current?.chat.chatId } as ChatIntent;
    if (!replacesSelectedChat(intent)) return maintenance.dispatch(captured, version);
    const result = navigationTail.current.then(() => maintenance.dispatch(captured, version));
    navigationTail.current = result.then(() => {}, () => {});
    return result;
  }, [maintenance.dispatch]);
  const pendingCommandIds = new Set([...allPendingCommandIds].filter(id => pendingRef.current.get(id) === snapshot?.chat.chatId));

  return {
    contextModel,
    snapshot,
    events,
    stale,
    loading,
    error,
    pendingCommandIds,
    maintenancePending: maintenance.pending(snapshot?.chat.chatId ?? ""),
    queuedMaintenanceInputs: maintenance.inputs(snapshot?.chat.chatId ?? ""),
    dispatch,
    resynchronize,
    dismissError,
  };
}

function replacesSelectedChat(intent: ChatIntent): boolean {
  return ["new_chat", "select_chat", "delete_chat", "fork"].includes(
    intent.type,
  );
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function bufferEvent(
  buffered: Map<string, RuntimeEvent>,
  event: RuntimeEvent,
): void {
  const key = `${event.streamId}:${event.sequence}`;
  const existing = buffered.get(key);
  if (existing !== undefined) assertSameEnvelope(existing, event);
  else buffered.set(key, event);
}

function drainContiguous(
  events: RuntimeEvent[],
  buffered: Map<string, RuntimeEvent>,
  streamId: string,
): void {
  for (;;) {
    const sequence = events.length + 1;
    const key = `${streamId}:${sequence}`;
    const event = buffered.get(key);
    if (event === undefined) return;
    buffered.delete(key);
    events.push(event);
  }
}

/** True when the slice appended after `from` holds only coalescible streamed content. */
function onlyStreamedContent(
  events: readonly RuntimeEvent[],
  from: number,
): boolean {
  for (let index = from; index < events.length; index += 1)
    if (events[index]?.kind !== "span.content_delta") return false;
  return true;
}

function mergeCanonicalEvents(
  current: readonly RuntimeEvent[],
  incoming: readonly RuntimeEvent[],
): RuntimeEvent[] {
  const bySequence = new Map<number, RuntimeEvent>();
  for (const event of [...current, ...incoming]) {
    const existing = bySequence.get(event.sequence);
    if (existing === undefined) bySequence.set(event.sequence, event);
    else assertSameEnvelope(existing, event);
  }
  const merged = [...bySequence.values()].sort(
    (left, right) => left.sequence - right.sequence,
  );
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
