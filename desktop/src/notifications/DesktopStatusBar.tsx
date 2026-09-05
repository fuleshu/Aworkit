import { useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore } from "react";
import { NotificationStore, primaryNotification } from "./NotificationStore";
import type { NotificationRecord, NotificationTiming } from "./types";
import "./notifications.css";

const symbols = { action: "!", error: "!", warning: "!", progress: "◌", success: "✓", info: "i" };

/** A docked shell row. The details panel reserves space instead of overlaying the workspace. */
export function DesktopStatusBar({ store, route }: { readonly store: NotificationStore; readonly route: string }): React.JSX.Element {
  const snapshot = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const primary = primaryNotification(snapshot.active, route);
  const [expanded, setExpanded] = useState(false);
  const [timing, setTiming] = useState<NotificationTiming>("default");
  const [hovered, setHovered] = useState(false);
  const [focused, setFocused] = useState(false);
  const listRef = useRef<HTMLButtonElement>(null);
  const dockRef = useRef<HTMLElement>(null);
  const focusedItem = useRef<string | null>(null);

  useEffect(() => { setExpanded(false); }, [route]);
  useEffect(() => {
    if (!primary || document.hidden) return;
    store.pause(primary.id, primary.occurrence, hovered || focused, "bar");
    return () => store.pause(primary.id, primary.occurrence, false, "bar");
  }, [store, primary?.id, primary?.occurrence, hovered, focused]);
  useLayoutEffect(() => {
    if (focusedItem.current !== null && !snapshot.active.some(item => item.id === focusedItem.current)) {
      listRef.current?.focus({ preventScroll: true });
      focusedItem.current = null;
    }
  }, [snapshot.active]);

  const collapse = () => { setExpanded(false); listRef.current?.focus({ preventScroll: true }); };
  const dismiss = (item: NotificationRecord) => {
    listRef.current?.focus({ preventScroll: true });
    store.dismiss(item.id, item.occurrence);
  };
  return (
    <section className="desktop-status-dock" aria-label="Application notifications" ref={dockRef}
      onFocusCapture={event => { focusedItem.current = (event.target as HTMLElement).closest<HTMLElement>("[data-notification-id]")?.dataset.notificationId ?? null; }}
      onKeyDown={event => { if (event.key === "Escape" && expanded) { event.preventDefault(); event.stopPropagation(); collapse(); } }}>
      {expanded && (
        <section className="notification-details" id="notification-details" aria-label="Notification details">
          <header><h2>Notifications</h2><button type="button" title="Close notification details" onClick={collapse}>Close</button></header>
          <h3>Active</h3>
          {snapshot.active.length === 0 ? <p className="notification-empty">No active notifications.</p> : (
            <ul>{snapshot.active.map(item => <NotificationItem key={`${item.id}:${item.occurrence}`} item={item} store={store} onDismiss={dismiss} />)}</ul>
          )}
          <details><summary>Recent</summary>
            {snapshot.recent.length === 0 ? <p className="notification-empty">No recent notifications.</p> : <ul>{snapshot.recent.map(item => (
              <li key={`${item.id}:${item.occurrence}`}><strong>{item.summary}</strong>{item.detail && <p>{item.detail}</p>}<small>{item.source} · {new Date(item.createdAt).toLocaleTimeString()}</small></li>
            ))}</ul>}
          </details>
          <label className="notification-timing">Message duration
            <select title="Notification reading time for this window; resolved messages always clear" value={timing} onChange={event => { const value = event.target.value as NotificationTiming; setTiming(value); store.setTiming(value); }}>
              <option value="default">Default</option><option value="extended">Extended</option><option value="manual">Dismiss manually</option>
            </select>
          </label>
        </section>
      )}
      <footer className={`desktop-status-bar severity-${primary?.severity ?? "info"}`}
        onPointerEnter={() => setHovered(true)} onPointerLeave={() => setHovered(false)}
        onFocusCapture={event => setFocused(!(event.target as HTMLElement).closest(".notification-list-toggle"))} onBlurCapture={event => { if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setFocused(false); }}>
        <div className="notification-primary" data-notification-id={primary?.id}>
          <span className="notification-symbol" aria-hidden="true">{primary ? symbols[primary.severity] : snapshot.active.length ? "!" : "✓"}</span>
          <span className="notification-message" role={primary?.severity === "action" || primary?.severity === "error" ? "alert" : "status"} aria-atomic="true"
            title={primary?.summary}>
            {primary?.summary ?? (snapshot.active.length ? `${snapshot.active.length} active ${snapshot.active.length === 1 ? "issue" : "issues"}` : "Ready")}
          </span>
        </div>
        {primary?.action && <button className="notification-primary-action" type="button" title={primary.action.label} disabled={primary.action.disabled} onClick={primary.action.run}>{primary.action.label}</button>}
        <button ref={listRef} type="button" className="notification-list-toggle" aria-label={`Notifications, ${snapshot.active.length} active`} aria-expanded={expanded} aria-controls={expanded ? "notification-details" : undefined} title="Show notifications and message duration" onClick={() => setExpanded(value => !value)}>
          <span aria-hidden="true">☷</span><span className="notification-list-label">Notifications</span><span>{snapshot.active.length}</span>
        </button>
        {primary && <button type="button" className="notification-dismiss" aria-label="Dismiss notification" title="Dismiss this message; ongoing conditions remain in the notification list" onClick={() => dismiss(primary)}>×</button>}
      </footer>
    </section>
  );
}

function NotificationItem({ item, store, onDismiss }: { readonly item: NotificationRecord; readonly store: NotificationStore; readonly onDismiss: (item: NotificationRecord) => void }): React.JSX.Element {
  const [hovered, setHovered] = useState(false);
  const [focused, setFocused] = useState(false);
  useEffect(() => {
    if (document.hidden) return;
    store.pause(item.id, item.occurrence, hovered || focused, "details");
    return () => store.pause(item.id, item.occurrence, false, "details");
  }, [store, item.id, item.occurrence, hovered, focused]);
  return <li data-notification-id={item.id} className={`severity-${item.severity}`}
    onPointerEnter={() => setHovered(true)} onPointerLeave={() => setHovered(false)}
    onFocusCapture={() => setFocused(true)} onBlurCapture={event => { if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setFocused(false); }}>
    <div className="notification-detail-title"><span className="notification-symbol" aria-hidden="true">{symbols[item.severity]}</span><strong>{item.summary}</strong></div>
    {item.detail && <p>{item.detail}</p>}<small>{item.source}{item.acknowledged ? " · acknowledged" : ""}</small>
    <div className="notification-item-actions">
      {item.action && <button type="button" title={item.action.label} disabled={item.action.disabled} onClick={item.action.run}>{item.action.label}</button>}
      {!item.acknowledged && <button type="button" title="Dismiss this notification" onClick={() => onDismiss(item)}>Dismiss</button>}
    </div>
  </li>;
}
