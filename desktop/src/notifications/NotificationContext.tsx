import { createContext, useContext, useEffect, useId, useMemo, useRef, type ReactNode } from "react";
import { NotificationStore } from "./NotificationStore";
import type { NotificationInput } from "./types";

const NotificationContext = createContext<NotificationStore | null>(null);

export function NotificationProvider({ store, children }: { readonly store: NotificationStore; readonly children: ReactNode }): React.JSX.Element {
  useEffect(() => {
    const resume = () => store.resume();
    document.addEventListener("visibilitychange", resume);
    window.addEventListener("pageshow", resume);
    window.addEventListener("blur", resume);
    return () => {
      document.removeEventListener("visibilitychange", resume);
      window.removeEventListener("pageshow", resume);
      window.removeEventListener("blur", resume);
    };
  }, [store]);
  return <NotificationContext.Provider value={store}>{children}</NotificationContext.Provider>;
}

export const useNotificationStore = (): NotificationStore | null => useContext(NotificationContext);

/** A stable feature port with generation fencing and explicit invalidation. */
export function useNotificationPublisher(source: string, scope: string, route?: string, active = true) {
  const store = useNotificationStore();
  const owner = useId();
  return useMemo(() => {
    const tokens = new Map<string, number>();
    return {
      publish(key: string, input: Omit<NotificationInput, "source" | "route">): void {
        if (!store || !active) return;
        const occurrence = store.nextOccurrence();
        tokens.set(key, occurrence);
        store.publish(`${owner}:${key}`, scope, occurrence, { ...input, source, route });
      },
      resolve(key: string): void {
        const occurrence = tokens.get(key);
        if (occurrence !== undefined) store?.resolve(`${owner}:${key}`, occurrence);
      },
      update(key: string, input: Partial<Pick<NotificationInput, "action" | "summary" | "detail" | "severity">>): void {
        const occurrence = tokens.get(key);
        if (occurrence !== undefined) store?.update(`${owner}:${key}`, occurrence, input);
      },
      clear(): void { tokens.forEach((occurrence, key) => store?.resolve(`${owner}:${key}`, occurrence)); },
    };
  }, [store, owner, scope, source, route, active]);
}

/** Projects conditions without rearming timers or acknowledgements on polling/rerenders. */
export function useProjectedNotification(
  source: string, scope: string, key: string, input: Omit<NotificationInput, "source"> | null, active = true, occurrence = 0,
): void {
  const publisher = useNotificationPublisher(source, scope, input?.route, active);
  const previous = useRef<string | null>(null);
  const action = useRef(input?.action);
  action.current = input?.action;
  const signature = input === null ? null : JSON.stringify([input.summary, input.detail, input.severity, input.lifetime]);
  const condition = input?.lifetime.kind !== "transient";
  const identity = input?.lifetime.kind === "condition" ? input.lifetime.conditionId
    : input?.lifetime.kind === "operation" ? input.lifetime.operationId : `${occurrence}:${signature}`;
  useEffect(() => {
    if (signature === null || !active) {
      publisher.resolve(key);
      if (signature === null) previous.current = null;
      return;
    }
    if (condition || previous.current !== identity) {
      publisher.publish(key, { ...input!, action: input?.action ? { ...input.action, run: () => action.current?.run() } : undefined });
    }
    previous.current = identity;
    return () => publisher.resolve(key);
    // Action identities are intentionally excluded; dispatch always uses the latest feature intent.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [publisher, identity, condition, active, key]);
  useEffect(() => {
    if (input) publisher.update(key, { summary: input.summary, detail: input.detail, severity: input.severity,
      action: input.action ? { ...input.action, run: () => action.current?.run() } : undefined });
  }, [publisher, key, signature, input?.action?.disabled, input?.action?.label]);
}
