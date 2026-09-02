import { useCallback, useEffect, useRef, useState } from "react";
import type { RuntimeEvent } from "./corePort";
import type { ErrorDialogNotice } from "./ErrorDialog";
import type { RuntimeErrorNotice } from "./useChatRuntime";

interface QueuedErrorNotice extends ErrorDialogNotice {
  readonly runtimeErrorId?: number;
}

/**
 * Queues command failures and newly committed terminal execution failures.
 * Replayed events are keyed by their canonical event ID, so polling or span
 * re-projection cannot reopen an acknowledged dialog.
 */
export function useChatErrorNotices(
  events: readonly RuntimeEvent[],
  eventsReady: boolean,
  runtimeError: RuntimeErrorNotice | null,
  dismissRuntimeError: () => void,
): {
  readonly notice: ErrorDialogNotice | null;
  dismiss(): void;
} {
  const [queue, setQueue] = useState<readonly QueuedErrorNotice[]>([]);
  const knownKeysRef = useRef(new Set<string>());
  const initializedEventsRef = useRef(false);

  const enqueue = useCallback((notice: QueuedErrorNotice): void => {
    if (knownKeysRef.current.has(notice.key)) return;
    knownKeysRef.current.add(notice.key);
    setQueue((current) => [...current, notice]);
  }, []);

  useEffect(() => {
    if (runtimeError === null) return;
    enqueue({
      key: `runtime-error.${runtimeError.id}`,
      title: "Aworkit error",
      body: runtimeError.message,
      runtimeErrorId: runtimeError.id,
    });
  }, [enqueue, runtimeError]);

  useEffect(() => {
    if (!eventsReady) return;
    const failures = events.filter(
      (event) => event.kind === "execution.failed",
    );
    if (!initializedEventsRef.current) {
      for (const event of failures)
        knownKeysRef.current.add(`execution-error.${event.eventId}`);
      initializedEventsRef.current = true;
      return;
    }
    for (const event of failures) {
      const payload = record(event.payload);
      enqueue({
        key: `execution-error.${event.eventId}`,
        title: string(payload.title) ?? "Execution failed",
        body:
          string(payload.body) ??
          "The run failed without a detailed error message. Inspect Run details for the source record.",
      });
    }
  }, [enqueue, events, eventsReady]);

  const dismiss = useCallback((): void => {
    const current = queue[0];
    if (
      current?.runtimeErrorId !== undefined &&
      runtimeError?.id === current.runtimeErrorId
    )
      dismissRuntimeError();
    setQueue((items) => items.slice(1));
  }, [dismissRuntimeError, queue, runtimeError]);

  return { notice: queue[0] ?? null, dismiss };
}

function record(value: unknown): Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}

function string(value: unknown): string | undefined {
  return typeof value === "string" && value.trim().length > 0
    ? value
    : undefined;
}
