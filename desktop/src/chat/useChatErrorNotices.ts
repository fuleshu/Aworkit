import { useEffect, useRef } from "react";
import { useNotificationPublisher } from "../notifications/NotificationContext";
import type { RuntimeEvent } from "./corePort";
import type { RuntimeErrorNotice } from "./useChatRuntime";

/** New failures become non-modal notices. Stream hydration and replay never reopen old errors. */
export function useChatErrorNotices(
  events: readonly RuntimeEvent[], eventsReady: boolean, chatId: string | null,
  runtimeError: RuntimeErrorNotice | null, pending: boolean, inspect: () => void,
): void {
  const notifications = useNotificationPublisher("Chat", `chat:${chatId ?? "startup"}`, "chat");
  const seenCommand = useRef<number | null>(null);
  const stream = useRef<string | null>(null);
  const through = useRef(0);
  const inspectRef = useRef(inspect);
  inspectRef.current = inspect;
  useEffect(() => {
    if (pending || runtimeError === null) { notifications.resolve("command-error"); return; }
    if (runtimeError.id === seenCommand.current) return;
    seenCommand.current = runtimeError.id;
    notifications.publish("command-error", {
      summary: runtimeError.message, severity: "error", lifetime: { kind: "transient" },
      action: { label: "Inspect", run: () => inspectRef.current() },
    });
  }, [notifications, pending, runtimeError]);
  useEffect(() => {
    if (!eventsReady || chatId === null || events.some(event => event.streamId !== chatId)) return;
    const head = events.at(-1)?.sequence ?? 0;
    if (stream.current !== chatId) { stream.current = chatId; through.current = head; return; }
    for (const event of events) {
      if (event.sequence <= through.current || event.kind !== "execution.failed") continue;
      const payload = event.payload as { title?: unknown; body?: unknown };
      const title = typeof payload.title === "string" ? payload.title : "Execution failed";
      const body = typeof payload.body === "string" ? payload.body : "Inspect Run details for the source record.";
      notifications.publish("execution-error", {
        summary: `${title}: ${body}`, severity: "error", lifetime: { kind: "transient" },
        action: { label: "Inspect Run details", run: () => inspectRef.current() },
      });
    }
    through.current = Math.max(through.current, head);
  }, [notifications, chatId, events, eventsReady]);
  useEffect(() => () => notifications.clear(), [notifications]);
}
