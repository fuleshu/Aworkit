import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { NotificationStore, primaryNotification } from "./NotificationStore";
import type { NotificationInput } from "./types";

const notice = (overrides: Partial<NotificationInput> = {}): NotificationInput => ({
  source: "Settings", summary: "Settings saved.", severity: "success", lifetime: { kind: "transient" }, ...overrides,
});
describe("notification lifecycle", () => {
  let store: NotificationStore;
  beforeEach(() => { vi.useFakeTimers(); store = new NotificationStore(); });
  afterEach(() => { store.dispose(); vi.useRealTimers(); });

  it("expires queued successes while an action occupies the primary slot", () => {
    store.publish("save", "settings:1", 1, notice());
    store.publish("action", "app", 2, notice({ severity: "action", lifetime: { kind: "condition", conditionId: "recovery" } }));
    expect(primaryNotification(store.getSnapshot().active, "settings")?.id).toBe("action");
    vi.advanceTimersByTime(5_000);
    expect(store.getSnapshot().active.map(item => item.id)).toEqual(["action"]);
    store.resolve("action", 2);
    expect(primaryNotification(store.getSnapshot().active, "settings")).toBeUndefined();
  });

  it("combines independent reading pauses, but source invalidation wins", () => {
    store.publish("save", "settings:1", 1, notice());
    vi.advanceTimersByTime(2_000);
    store.pause("save", 1, true, "bar");
    store.pause("save", 1, true, "details");
    vi.advanceTimersByTime(10_000);
    store.pause("save", 1, false, "bar");
    vi.advanceTimersByTime(10_000);
    expect(store.getSnapshot().active).toHaveLength(1);
    store.pause("save", 1, false, "details");
    vi.advanceTimersByTime(2_999);
    expect(store.getSnapshot().active).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(store.getSnapshot().active).toHaveLength(0);
    store.publish("save", "settings:1", 2, notice());
    store.pause("save", 2, true);
    store.closeScope("settings:1");
    expect(store.getSnapshot().active).toHaveLength(0);
  });

  it("expires backgrounded messages and rejects late closed-visit completions", () => {
    store.publish("save", "settings:1", 1, notice());
    store.pause("save", 1, true);
    store.resume();
    vi.advanceTimersByTime(5_001);
    store.resume();
    expect(store.getSnapshot().active).toHaveLength(0);
    store.closeScope("settings:1");
    store.publish("save", "settings:1", 99, notice());
    expect(store.getSnapshot().active).toHaveLength(0);
  });

  it("retains acknowledged conditions without rearming, then permits real recurrence", () => {
    const failure = notice({ severity: "warning", lifetime: { kind: "condition", conditionId: "connection" } });
    store.publish("connection", "app", 1, failure);
    store.dismiss("connection", 1);
    store.update("connection", 1, { detail: "New polling detail" });
    store.publish("connection", "app", 1, failure);
    expect(store.getSnapshot().active).toHaveLength(1);
    expect(primaryNotification(store.getSnapshot().active, "chat")).toBeUndefined();
    store.resolve("connection", 1);
    store.publish("connection", "app", 2, failure);
    expect(primaryNotification(store.getSnapshot().active, "chat")?.acknowledged).toBe(false);
  });

  it("rejects old updates, completions, and replay after replacement and expiry", () => {
    store.publish("save", "settings:1", 1, notice());
    store.publish("save", "settings:1", 2, notice({ summary: "New save" }));
    store.resolve("save", 1);
    store.update("save", 1, { summary: "Old failure" });
    expect(store.getSnapshot().active[0]?.summary).toBe("New save");
    vi.advanceTimersByTime(6_000);
    store.publish("save", "settings:1", 2, notice());
    expect(store.getSnapshot().active).toHaveLength(0);
  });

  it("bounds recent text and drops retained actions without replay", () => {
    for (let index = 1; index <= 60; index++) {
      store.publish("save", "app", index, notice({ action: { label: "Inspect", run: vi.fn() } }));
      store.resolve("save", index);
    }
    expect(store.getSnapshot().recent).toHaveLength(50);
    expect(store.getSnapshot().recent.every(item => item.action === undefined)).toBe(true);
    vi.advanceTimersByTime(24 * 60 * 60 * 1000);
    expect(store.getSnapshot().recent).toHaveLength(0);
  });

  it("lets accessibility timing extend reading without retaining resolved sources", () => {
    store.setTiming("extended");
    store.publish("save", "settings:1", 1, notice());
    vi.advanceTimersByTime(14_999);
    expect(store.getSnapshot().active).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(store.getSnapshot().active).toHaveLength(0);
    store.setTiming("manual");
    store.publish("save", "settings:1", 2, notice());
    vi.advanceTimersByTime(60_000);
    expect(store.getSnapshot().active).toHaveLength(1);
    store.clearScope("settings:1");
    expect(store.getSnapshot().active).toHaveLength(0);
  });
});
