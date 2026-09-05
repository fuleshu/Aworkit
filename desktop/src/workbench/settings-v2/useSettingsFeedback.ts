import { useCallback, useEffect } from "react";
import { useNotificationPublisher } from "../../notifications/NotificationContext";

export interface SettingsFeedback { readonly tone: "error" | "success" | "warning"; readonly message: string }

/** Settings operation feedback is owned by a visit, with no retained overlay state. */
export function useSettingsFeedback(scope: string, active: boolean, busy: string | null) {
  const notifications = useNotificationPublisher("Settings", scope, "settings", active);
  const setFeedback = useCallback((value: SettingsFeedback | null) => {
    if (value === null) notifications.resolve("result");
    else notifications.publish("result", { summary: value.message, severity: value.tone, lifetime: { kind: "transient" } });
  }, [notifications]);
  useEffect(() => {
    if (busy === null) return;
    notifications.publish("operation", {
      summary: busy === "save" ? "Saving configuration…" : busy === "discard" ? "Restoring saved configuration…" : "Updating Settings…",
      severity: "progress", lifetime: { kind: "operation", operationId: `${scope}:${busy}` },
    });
    // A slow/lost response must not leave a misleading endless activity spinner.
    const timer = setTimeout(() => notifications.update("operation", {
      summary: "Still waiting for Settings. The outcome has not been confirmed.", severity: "warning",
    }), 30_000);
    return () => { clearTimeout(timer); notifications.resolve("operation"); };
  }, [notifications, busy, scope]);
  useEffect(() => () => notifications.clear(), [notifications]);
  return setFeedback;
}
