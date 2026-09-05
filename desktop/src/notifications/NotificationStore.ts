import { notificationDuration, type NotificationInput, type NotificationRecord, type NotificationSnapshot, type NotificationTiming } from "./types";

const DAY = 24 * 60 * 60 * 1000;
const priority = { action: 6, error: 5, warning: 4, progress: 3, success: 2, info: 2 };

/** One window's disposable notification projection. Never dispatches domain commands. */
export class NotificationStore {
  private snapshot: NotificationSnapshot = { active: [], recent: [] };
  private listeners = new Set<() => void>();
  private timer: ReturnType<typeof setTimeout> | undefined;
  private sequence = 0;
  private closedScopes = new Set<string>();
  private watermarks = new Map<string, number>();
  private paused = new Map<string, { started: number; owners: Set<string> }>();
  private timing: NotificationTiming = "default";

  getSnapshot = (): NotificationSnapshot => this.snapshot;
  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };
  nextOccurrence = (): number => ++this.sequence;

  /** Caller supplies an occurrence token, so stale promises and replay cannot revive a notice. */
  publish(id: string, scope: string, occurrence: number, input: NotificationInput): void {
    if (this.closedScopes.has(scope) || occurrence <= (this.watermarks.get(id) ?? 0)) return;
    this.watermarks.set(id, occurrence);
    const old = this.snapshot.active.find(item => item.id === id);
    const now = Date.now();
    const duration = input.lifetime.kind === "transient"
      ? input.lifetime.durationMs ?? notificationDuration[input.severity] : null;
    const record: NotificationRecord = {
      ...input, id, scope, occurrence, createdAt: now, acknowledged: false,
      expiresAt: duration === null || this.timing === "manual" ? null
        : now + duration * (this.timing === "extended" ? 3 : 1),
    };
    this.paused.delete(id);
    this.commit([...this.snapshot.active.filter(item => item.id !== id), record], old ? [old] : []);
  }

  update(id: string, occurrence: number, patch: Partial<Pick<NotificationInput, "summary" | "detail" | "action" | "severity">>): void {
    const current = this.snapshot.active.find(item => item.id === id && item.occurrence === occurrence);
    if (!current) return;
    this.commit(this.snapshot.active.map(item => item === current ? { ...item, ...patch } : item));
  }

  resolve(id: string, occurrence: number): void {
    const current = this.snapshot.active.find(item => item.id === id && item.occurrence === occurrence);
    if (!current) return;
    this.paused.delete(id);
    this.commit(this.snapshot.active.filter(item => item !== current), [current]);
  }

  dismiss(id: string, occurrence: number): void {
    const current = this.snapshot.active.find(item => item.id === id && item.occurrence === occurrence);
    if (!current) return;
    if (current.lifetime.kind === "transient") this.resolve(id, occurrence);
    else this.updateAcknowledgement(current);
  }

  private updateAcknowledgement(current: NotificationRecord): void {
    this.commit(this.snapshot.active.map(item => item === current ? { ...item, acknowledged: true } : item));
  }

  clearScope(scope: string): void {
    const removed = this.snapshot.active.filter(item => item.scope === scope);
    removed.forEach(item => this.paused.delete(item.id));
    if (removed.length) this.commit(this.snapshot.active.filter(item => item.scope !== scope), removed);
  }

  /** A visit is closed even while React keeps its feature mounted. Late publications are rejected. */
  closeScope(scope: string): void {
    this.closedScopes.add(scope);
    this.clearScope(scope);
  }

  pause(id: string, occurrence: number, paused: boolean, owner = "default"): void {
    const current = this.snapshot.active.find(item => item.id === id && item.occurrence === occurrence);
    if (!current || current.expiresAt === null) return;
    if (paused) {
      if (!this.paused.has(id)) this.paused.set(id, { started: Date.now(), owners: new Set() });
      this.paused.get(id)!.owners.add(owner);
      this.schedule();
    } else {
      const lease = this.paused.get(id);
      if (lease === undefined) return;
      lease.owners.delete(owner);
      if (lease.owners.size > 0) return;
      this.paused.delete(id);
      this.commit(this.snapshot.active.map(item => item === current
        ? { ...item, expiresAt: current.expiresAt! + Date.now() - lease.started } : item));
    }
  }

  /** Hidden windows must not keep hover/focus pauses alive. */
  resume = (): void => {
    for (const item of this.snapshot.active) {
      for (const owner of this.paused.get(item.id)?.owners ?? []) this.pause(item.id, item.occurrence, false, owner);
    }
    this.expire();
  };

  setTiming(timing: NotificationTiming): void {
    this.resume();
    this.timing = timing;
    this.commit(this.snapshot.active.map(item => item.lifetime.kind !== "transient" ? item : {
      ...item,
      expiresAt: timing === "manual" ? null : Date.now()
        + (item.lifetime.durationMs ?? notificationDuration[item.severity]) * (timing === "extended" ? 3 : 1),
    }));
  }

  private expire = (): void => {
    const now = Date.now();
    const expired = this.snapshot.active.filter(item => item.expiresAt !== null && item.expiresAt <= now && !this.paused.has(item.id));
    this.commit(this.snapshot.active.filter(item => !expired.includes(item)), expired);
  };

  private commit(active: readonly NotificationRecord[], removed: readonly NotificationRecord[] = []): void {
    // Recent holds text only: retaining callbacks would retain old feature trees and stale actions.
    const recent = [...removed.map(({ action: _action, ...item }) => item), ...this.snapshot.recent]
      .filter(item => item.createdAt > Date.now() - DAY).slice(0, 50);
    this.snapshot = { active, recent };
    this.schedule();
    this.listeners.forEach(listener => listener());
  }

  private schedule(): void {
    clearTimeout(this.timer);
    const deadlines = this.snapshot.active.filter(item => item.expiresAt !== null && !this.paused.has(item.id)).map(item => item.expiresAt!);
    if (this.snapshot.recent.length) deadlines.push(...this.snapshot.recent.map(item => item.createdAt + DAY));
    if (deadlines.length) this.timer = setTimeout(this.expire, Math.max(1, Math.min(...deadlines) - Date.now()));
  }

  dispose(): void {
    clearTimeout(this.timer);
    this.listeners.clear();
  }
}

export function primaryNotification(items: readonly NotificationRecord[], route: string): NotificationRecord | undefined {
  return items.filter(item => !item.acknowledged).sort((a, b) =>
    priority[b.severity] - priority[a.severity]
    || Number(b.route === route) - Number(a.route === route)
    || (a.severity === "action" ? a.occurrence - b.occurrence : b.occurrence - a.occurrence),
  )[0];
}
