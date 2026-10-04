import { useEffect, useRef } from "react";
import { useNotificationPublisher } from "../notifications/NotificationContext";
import type { RuntimeEvent } from "./corePort";

/**
 * Each distinct unavailable-capability warning becomes one standing status-bar
 * notice instead of repeated Chat content: the recorded Chat re-emitted the same
 * six warnings on every one of seven requests.
 *
 * Only newly committed events notify, following the Chat failure notices, so
 * hydrating or replaying a Chat never reopens an old warning. The notice has a
 * condition lifetime keyed by the warning message, so a dismissal is not
 * reopened by the same event and a cleared condition (committed by the runtime)
 * removes the notice until the condition genuinely returns.
 */
export function useCapabilityWarningNotices(
  events: readonly RuntimeEvent[], eventsReady: boolean, chatId: string | null,
): void {
  const notifications = useNotificationPublisher("Chat", `chat:${chatId ?? "startup"}`, "chat");
  const stream = useRef<string | null>(null);
  const through = useRef(0);
  useEffect(() => {
    if (!eventsReady || chatId === null || events.some(event => event.streamId !== chatId)) return;
    const head = events.at(-1)?.sequence ?? 0;
    // The first observation of a Chat is hydration or replay: the committed
    // record is shown in Run details, not reopened as a new notice.
    if (stream.current !== chatId) { stream.current = chatId; through.current = head; return; }
    for (const event of events) {
      if (event.sequence <= through.current) continue;
      if (event.kind === "workflow.capability-warning") {
        const message = warningMessage(event);
        if (message === undefined) continue;
        notifications.publish(message, {
          summary: message, severity: "warning",
          lifetime: { kind: "condition", conditionId: `capability-warning:${message}` },
        });
      } else if (event.kind === "workflow.capability-warning-cleared") {
        const message = warningMessage(event);
        if (message !== undefined) notifications.resolve(message);
      }
    }
    through.current = Math.max(through.current, head);
  }, [notifications, chatId, events, eventsReady]);
  useEffect(() => () => notifications.clear(), [notifications]);
}

function warningMessage(event: RuntimeEvent): string | undefined {
  const message = (event.payload as { message?: unknown }).message;
  return typeof message === "string" && message.length > 0 ? message : undefined;
}
