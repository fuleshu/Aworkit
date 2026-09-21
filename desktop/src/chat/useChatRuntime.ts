import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ChatIntent } from "./types";
import { useChatMaintenance } from "./useChatMaintenance";
import { assertContiguous, assertSameEnvelope, mergeCanonicalEvents, withEventSupport } from "./eventWindow";
import { useOlderChatEvents } from "./useOlderChatEvents";
import {
  createChatCorePort,
  type ChatCorePort,
  type RuntimeEvent,
  type RuntimeSnapshot,
} from "./corePort";

export interface ChatRuntimeState {
  /** The exact port this projection was built from, for scoped reads. */
  readonly port: ChatCorePort;
  readonly contextModel?: ChatCorePort["contextModel"];
  readonly snapshot: RuntimeSnapshot | null;
  readonly events: readonly RuntimeEvent[];
  readonly stale: boolean;
  readonly loading: boolean;
  readonly firstSequence: number;
  readonly hasOlderEvents: boolean;
  readonly olderLoading: boolean;
  readonly olderError: string | null;
  loadOlder(): Promise<void>;
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
  const supportRef = useRef<RuntimeEvent[]>([]);
  const firstSequenceRef = useRef(1);
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
      firstSequenceRef.current = next[0]?.sequence ?? 1;
      setEvents(withEventSupport(next, supportRef.current));
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
      setEvents(withEventSupport(eventsRef.current, supportRef.current));
    }, STREAM_PUBLISH_INTERVAL_MS);
  }, []);

  useEffect(() => cancelDeferredPublish, [cancelDeferredPublish]);

  const ingestLiveEvent = useCallback(
    (event: RuntimeEvent): void => {
      try {
        if (!initializedRef.current || navigatingRef.current) {
          bufferEvent(bufferedRef.current, event);
          return;
        }
        // The native subscription is process-wide. A delayed envelope from a
        // previously selected Chat is valid history, but it does not belong in
        // the active stream projection and must not collide by sequence.
        if (event.streamId !== snapshotRef.current?.chat.chatId) return;
        if (event.sequence < firstSequenceRef.current) return;
        const existing = eventsRef.current.find(item => item.sequence === event.sequence);
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
      if ([...next.events, ...(next.eventWindow?.supportingEvents ?? [])].some((event) => event.streamId !== next.chat.chatId || event.branchId !== "main" || event.sequence > next.throughSequence)) {
        throw new Error("trusted-core snapshot contains a foreign Chat stream");
      }
      const changedChat = full && next.chat.chatId !== snapshotRef.current?.chat.chatId;
      const base = changedChat ? [] : eventsRef.current;
      const first = base[0]?.sequence ?? next.eventWindow?.firstSequence ?? 1;
      const merged = mergeCanonicalEvents(base, next.events, first);
      if (full && (merged.at(-1)?.sequence ?? 0) < next.throughSequence) {
        throw new Error(
          `projection gap: snapshot ended at ${merged.length}, head is ${next.throughSequence}`,
        );
      }
      const buffered = [...bufferedRef.current.values()].sort(
        (left, right) => left.sequence - right.sequence,
      ).filter((event) => event.streamId === next.chat.chatId);
      for (const event of buffered) if (event.sequence >= first) mergeOne(merged, event);
      bufferedRef.current.clear();
      merged.sort((a,b) => a.sequence - b.sequence);
      assertContiguous(merged, first);
      supportRef.current = withEventSupport(changedChat ? [] : supportRef.current, next.eventWindow?.supportingEvents ?? []);
      initializedRef.current = true;
      snapshotRef.current = next;
      setError(errorsRef.current.get(next.chat.chatId) ?? null);
      setSnapshot(next);
      // Metadata polling must not rebuild a long transcript or its contexts
      // when all canonical envelopes are already the same objects.
      if (changedChat || merged.length !== eventsRef.current.length || merged.some((event, index) => event !== eventsRef.current[index])) {
        publishEvents(merged);
      }
    },
    [publishEvents],
  );

  const resynchronize = useCallback(async (replaceStream = false, selectedChatId?: string): Promise<boolean> => {
    const generation = generationRef.current;
    const request = ++snapshotRequestsRef.current.requested;
    const chatId = selectedChatId ?? (replaceStream ? undefined : snapshotRef.current?.chat.chatId);
    try {
      // The retained prefix is already contiguous committed history. Recover
      // its exact tail rather than retransmitting a long Chat after each action.
      const afterSequence = replaceStream || chatId === undefined ? 0 : (eventsRef.current.at(-1)?.sequence ?? 0);
      const next = await port.snapshot(afterSequence, chatId);
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
      if (generation === generationRef.current) setLoading(false);
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
      if (current === null || (stale && !replacesSelectedChat(intent))) return false;
      const navigation = replacesSelectedChat(intent);
      if (navigatingRef.current && !navigation) return false;
      const chatId = intent.targetId ?? current.chat.chatId;
      pendingRef.current.set(intent.commandId, chatId);
      setPending((value) => new Set([...value, intent.commandId]));
      if (navigation) {
        cancelDeferredPublish();
        generationRef.current += 1; navigatingRef.current = true; setLoading(true); setStale(false); setError(null);
        const entry = current.history.find(entry => entry.chatId === intent.targetId);
        if (intent.type === "select_chat" && entry) {
          setSnapshot({ ...current, contextModel: null, evidence: [], events: [], subagents: [], chat: {
            ...current.chat, chatId: entry.chatId, runId: entry.runId, title: entry.title,
            scope: entry.projectName ?? "No project", projectId: entry.projectId, workflowId: null, workflowName: null,
            branch: null, recoveryPending: false, phase: entry.phase, queuedInputs: [],
          } });
          setEvents([]);
        }
      }
      const generation = generationRef.current;
      const commandError = (failure: unknown) => {
        const notice = { id: ++nextErrorIdRef.current, message: message(failure) };
        errorsRef.current.set(chatId, notice);
        if (snapshotRef.current?.chat.chatId === chatId) setError(notice);
      };
      try {
        await eventReadyRef.current;
        const version = expectedVersion ?? (navigation || chatId === current.chat.chatId ? current.version : (await port.snapshot(0, chatId)).version);
        const receiptPromise = navigation ? navigationTail.current.then(() => port.command(intent, version)) : port.command(intent, version);
        if (navigation) navigationTail.current = receiptPromise.then(() => {}, () => {});
        const receipt = await receiptPromise;
        if (generation !== generationRef.current) return true;
        if (!receipt.accepted) {
          const reason =
            receipt.reason ?? "The trusted core rejected the command.";
          await resynchronize(replacesSelectedChat(intent));
          commandError(reason);
          return false;
        }
        errorsRef.current.delete(chatId);
        await resynchronize(navigation, intent.type === "select_chat" ? intent.targetId : undefined);
        return true;
      } catch (failure) {
        if (generation !== generationRef.current) return true;
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
        if (navigation && generation === generationRef.current) { navigatingRef.current = false; setLoading(false); }
        pendingRef.current.delete(intent.commandId);
        setPending((value) => {
          const next = new Set(value);
          next.delete(intent.commandId);
          return next;
        });
      }
    },
    [port, resynchronize, stale, cancelDeferredPublish],
  );

  const maintenance = useChatMaintenance(execute);
  const dispatch = useCallback((intent: ChatIntent, version?: number): Promise<boolean> => {
    const captured = { ...intent, targetId: intent.targetId ?? snapshotRef.current?.chat.chatId } as ChatIntent;
    return maintenance.dispatch(captured, version);
  }, [maintenance.dispatch]);
  const pendingCommandIds = new Set([...allPendingCommandIds].filter(id => pendingRef.current.get(id) === snapshot?.chat.chatId));
  const older = useOlderChatEvents(port, snapshotRef, eventsRef, supportRef, generationRef, publishEvents);

  return {
    port,
    contextModel,
    snapshot,
    events,
    stale,
    loading,
    firstSequence: firstSequenceRef.current,
    hasOlderEvents: !!port.olderEvents && firstSequenceRef.current > 1,
    olderLoading: older.loading,
    olderError: older.error,
    loadOlder: older.load,
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
    const sequence = (events.at(-1)?.sequence ?? 0) + 1;
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

function mergeOne(base: RuntimeEvent[], event: RuntimeEvent): void {
  const existing = base.find((candidate) => candidate.sequence === event.sequence);
  if (existing !== undefined) {
    assertSameEnvelope(existing, event);
    return;
  }
  base.push(event);
}
