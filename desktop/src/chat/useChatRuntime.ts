import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ChatIntent } from "./types";
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
  const bufferedRef = useRef<Map<number, RuntimeEvent>>(new Map());
  const initializedRef = useRef(false);
  const [events, setEvents] = useState<readonly RuntimeEvent[]>([]);
  const pendingRef = useRef<Set<string>>(new Set());
  const maintenanceRef = useRef(false);
  const drainingRef = useRef(false);
  const queueRef = useRef<Array<{chatId:string; intent:Extract<ChatIntent,{type:"enqueue"}>}>>([]);
  const [maintenancePending,setMaintenancePending] = useState(false);
  const [queueVersion,setQueueVersion] = useState(0);
  const eventReadyRef = useRef<Promise<void>>(Promise.resolve());
  const nextErrorIdRef = useRef(0);
  const lastFailureRef = useRef<string | null>(null);
  const [stale, setStale] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<RuntimeErrorNotice | null>(null);
  const [pendingCommandIds, setPending] = useState<ReadonlySet<string>>(
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
    setError(null);
  }, []);

  const failProjection = useCallback(
    (failure: unknown): void => {
      setStale(true);
      reportError(failure);
    },
    [reportError],
  );

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
      if (next.events.some((event) => event.streamId !== next.chat.chatId)) {
        throw new Error("trusted-core snapshot contains a foreign Chat stream");
      }
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
      ).filter((event) => event.streamId === next.chat.chatId);
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

  const resynchronize = useCallback(async (replaceStream = false): Promise<boolean> => {
    try {
      const next = await port.snapshot(0);
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
      setStale(false);
      markHealthy();
      return true;
    } catch (failure) {
      failProjection(failure);
      return false;
    } finally {
      setLoading(false);
    }
  }, [failProjection, markHealthy, port, replaceSnapshot]);

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
      markHealthy();
    } catch (failure) {
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
      pendingRef.current.add(intent.commandId);
      setPending((value) => new Set([...value, intent.commandId]));
      try {
        await eventReadyRef.current;
        const receipt = await port.command(intent, expectedVersion ?? current.version);
        if (!receipt.accepted) {
          const reason =
            receipt.reason ?? "The trusted core rejected the command.";
          await resynchronize(replacesSelectedChat(intent));
          reportError(reason);
          return false;
        }
        return await resynchronize(replacesSelectedChat(intent));
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
        reportError(failureMessage);
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
    [port, reportError, resynchronize, stale],
  );

  const dispatch = useCallback(async(intent:ChatIntent,expectedVersion?:number):Promise<boolean>=>{
    if (intent.type === "enqueue" && (maintenanceRef.current || drainingRef.current || queueRef.current.length > 0)) {
      const current=snapshotRef.current;
      if (!current || stale || current.chat.recoveryPending) return false;
      if (!queueRef.current.some(entry=>entry.intent.commandId===intent.commandId)) {
        queueRef.current.push({chatId:current.chat.chatId,intent});
        setQueueVersion(v=>v+1);
      }
      return true;
    }
    if (intent.type !== "compact_context") return execute(intent,expectedVersion);
    maintenanceRef.current=true;setMaintenancePending(true);
    try { return await execute(intent,expectedVersion); }
    finally { maintenanceRef.current=false;setMaintenancePending(false); }
  },[execute,stale]);

  useEffect(()=>{
    if (maintenancePending || drainingRef.current || stale || !queueRef.current.length || snapshot?.chat.recoveryPending) return;
    drainingRef.current=true;
    void (async()=>{
      try {
        while(queueRef.current.length) {
          const entry=queueRef.current[0],current=snapshotRef.current;
          if (!current || current.chat.chatId!==entry.chatId || current.chat.recoveryPending) break;
          // Each command gets the settled predecessor's current history fence.
          // Failed/uncertain admission retains its original ID and queued text.
          if (!await execute(entry.intent)) break;
          queueRef.current.shift();setQueueVersion(v=>v+1);
        }
      } finally {drainingRef.current=false;}
    })();
  },[maintenancePending,queueVersion,stale,execute,snapshot?.chat.chatId,snapshot?.chat.recoveryPending]);

  return {
    contextModel,
    snapshot,
    events,
    stale,
    loading,
    error,
    pendingCommandIds,
    maintenancePending,
    queuedMaintenanceInputs:queueRef.current.filter(entry=>entry.chatId===snapshot?.chat.chatId).map(entry=>entry.intent.input),
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
