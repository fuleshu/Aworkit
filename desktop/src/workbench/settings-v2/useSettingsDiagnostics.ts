import { useCallback, useEffect, useRef } from "react";
import { useNotificationPublisher } from "../../notifications/NotificationContext";

interface DiagnosticResult { readonly ok?: boolean; readonly message?: string }

/** Read-only diagnostics detach on draft/visit changes; late results cannot revive feedback. */
export function useSettingsDiagnostics(scope: string, active: boolean, fingerprint: string, generation = 0) {
  const publisher = useNotificationPublisher("Settings diagnostics", scope, "settings", active);
  const context = `${scope}:${active}:${generation}`;
  const current = useRef(context);
  current.current = context;
  const latestFingerprint = useRef(fingerprint);
  latestFingerprint.current = fingerprint;
  const attempts = useRef(new Map<string, symbol>());
  useEffect(() => () => { publisher.clear(); attempts.current.clear(); }, [publisher, context]);
  useEffect(() => () => publisher.clear(), [publisher, fingerprint]);
  return useCallback(async <Result extends DiagnosticResult,>(label: string, operation: () => Promise<Result>): Promise<Result> => {
    const token = Symbol(label);
    attempts.current.set(label, token);
    const relevant = () => active && current.current === context && attempts.current.get(label) === token;
    const feedbackRelevant = () => relevant() && latestFingerprint.current === fingerprint;
    publisher.publish(label, { summary: `${label} running…`, severity: "progress", lifetime: { kind: "operation", operationId: label } });
    const timer = setTimeout(() => {
      if (feedbackRelevant()) publisher.update(label, { summary: `${label}: still waiting for a result.`, severity: "warning" });
    }, 30_000);
    try {
      const result = await operation();
      if (!relevant()) throw new Error("This Settings diagnostic is no longer relevant. Run it again for the current draft.");
      if (feedbackRelevant()) publisher.publish(label, {
        summary: `${label} ${result.ok === false ? "failed" : "completed"}. See diagnostic details.`,
        severity: result.ok === false ? "error" : "success", lifetime: { kind: "transient" },
      });
      return result;
    } catch (error) {
      if (feedbackRelevant()) publisher.publish(label, { summary: `${label} failed. See diagnostic details.`, severity: "error", lifetime: { kind: "transient" } });
      throw error;
    } finally { clearTimeout(timer); }
  }, [publisher, context, active, fingerprint]);
}
