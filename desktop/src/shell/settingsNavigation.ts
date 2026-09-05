import { useCallback, useEffect, useRef, useState, type RefObject } from "react";
import type { NotificationStore } from "../notifications/NotificationStore";
import type { Route } from "./NavigationPane";

export type SettingsLeaveGuard = (leave: () => void) => void;
interface ViewReturnContext {
  readonly route: Route;
  readonly focus: HTMLElement | null;
  readonly selection: readonly [number | null, number | null] | null;
  readonly scroll: readonly { element: HTMLElement; top: number; left: number; follow: boolean }[];
}

function captureView(route: Route, main: HTMLElement | null): ViewReturnContext {
  const focus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const selection = focus instanceof HTMLInputElement || focus instanceof HTMLTextAreaElement
    ? [focus.selectionStart, focus.selectionEnd] as const : null;
  const surface = main?.querySelector<HTMLElement>(`[data-route="${route}"]`);
  const elements = surface ? [surface, ...surface.querySelectorAll<HTMLElement>("*")] : [];
  return {
    route, focus, selection,
    scroll: elements.filter(element => element.scrollTop !== 0 || element.scrollLeft !== 0 || element.scrollHeight > element.clientHeight)
      .map(element => ({ element, top: element.scrollTop, left: element.scrollLeft, follow: element.dataset.followLatest === "true" })),
  };
}

/** Owns one Settings visit, never a second copy of feature/domain state. */
export function useSettingsNavigation(mainRef: RefObject<HTMLElement | null>, store: NotificationStore) {
  const [route, setRoute] = useState<Route>("chat");
  const routeRef = useRef(route);
  const [visit, setVisit] = useState(0);
  const visitRef = useRef(0);
  const origin = useRef<ViewReturnContext | null>(null);
  const guard = useRef<SettingsLeaveGuard | null>(null);
  const [mountedRoutes, setMountedRoutes] = useState<ReadonlySet<Route>>(new Set(["chat"]));
  const frames = useRef<number[]>([]);
  useEffect(() => () => frames.current.forEach(window.cancelAnimationFrame), []);

  const commit = useCallback((next: Route, after?: () => void, restore = false) => {
    const previous = routeRef.current;
    if (previous === "settings" && next !== "settings") store.closeScope(`settings:${visitRef.current}`);
    if (next === "settings" && previous !== "settings") {
      origin.current = captureView(previous, mainRef.current);
      visitRef.current += 1;
      setVisit(visitRef.current);
    }
    routeRef.current = next;
    setMountedRoutes(current => new Set([...current, next]));
    setRoute(next);
    frames.current.forEach(window.cancelAnimationFrame);
    frames.current = [window.requestAnimationFrame(() => {
      const context = restore ? origin.current : null;
      if (context && context.route === next) {
        for (const { element, top, left, follow } of context.scroll) {
          if (!element.isConnected) continue;
          element.scrollLeft = left;
          // Timeline owns follow-latest and its remeasurement; manual scrolling is restored here.
          if (!follow) element.scrollTop = top;
        }
        const focus = context.focus;
        if (focus?.isConnected && !focus.closest("[hidden]")) {
          focus.focus({ preventScroll: true });
          if (context.selection && (focus instanceof HTMLInputElement || focus instanceof HTMLTextAreaElement)) {
            try { focus.setSelectionRange(...context.selection); } catch { /* Non-text inputs have no caret. */ }
          }
        } else mainRef.current?.focus({ preventScroll: true });
        origin.current = null;
      } else mainRef.current?.focus({ preventScroll: true });
      after?.();
    })];
  }, [mainRef, store]);

  const navigate = useCallback((next: Route, after?: () => void) => {
    if (next === "settings" && routeRef.current === "settings") return;
    if (routeRef.current === "settings" && next !== "settings" && guard.current) guard.current(() => commit(next, after));
    else commit(next, after);
  }, [commit]);
  const back = useCallback(() => {
    const destination = origin.current?.route ?? "chat";
    const leave = () => commit(destination, undefined, true);
    if (guard.current) guard.current(leave); else leave();
  }, [commit]);
  const registerLeaveGuard = useCallback((next: SettingsLeaveGuard | null) => { guard.current = next; }, []);
  return { route, mountedRoutes, visit, navigate, back, registerLeaveGuard, returnLabel: `Back to ${origin.current?.route === "workflows" ? "Workflows" : origin.current?.route === "management" ? "Management Chat" : "Chat"}` };
}
