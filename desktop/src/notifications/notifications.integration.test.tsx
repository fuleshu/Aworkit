// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DesktopStatusBar } from "./DesktopStatusBar";
import { NotificationProvider, useProjectedNotification } from "./NotificationContext";
import { NotificationStore } from "./NotificationStore";

afterEach(() => { cleanup(); vi.useRealTimers(); });
describe("desktop notification presentation", () => {
  it("updates condition detail without reopening an acknowledgement on polling", () => {
    const store = new NotificationStore();
    function Source({ failing, detail }: { failing: boolean; detail: string }) {
      useProjectedNotification("Chat", "app", "connection", failing ? {
        summary: "Disconnected", detail, severity: "warning", lifetime: { kind: "condition", conditionId: "connection" },
      } : null);
      return null;
    }
    const view = (failing: boolean, detail: string) => <NotificationProvider store={store}><Source failing={failing} detail={detail} /><DesktopStatusBar store={store} route="chat" /></NotificationProvider>;
    const { rerender } = render(view(true, "First poll"));
    fireEvent.click(screen.getByRole("button", { name: "Dismiss notification" }));
    rerender(view(true, "Second poll"));
    expect(screen.queryByText("Disconnected")).toBeNull();
    expect(screen.getByRole("button", { name: "Notifications, 1 active" })).toBeVisible();
    rerender(view(false, "Recovered"));
    expect(screen.getByText("Ready")).toBeVisible();
    rerender(view(true, "New failure"));
    expect(screen.getByText("Disconnected")).toBeVisible();
    store.dispose();
  });

  it("keeps focus in details when a focused source resolves; Recent cannot invoke actions", () => {
    const store = new NotificationStore();
    store.publish("failure", "app", 1, { summary: "Failed", source: "Chat", severity: "error", lifetime: { kind: "transient" }, action: { label: "Inspect failure", run: vi.fn() } });
    render(<DesktopStatusBar store={store} route="chat" />);
    const list = screen.getByRole("button", { name: "Notifications, 1 active" });
    fireEvent.click(list);
    const details = screen.getByRole("region", { name: "Notification details" });
    act(() => within(details).getByRole("button", { name: "Inspect failure" }).focus());
    act(() => store.resolve("failure", 1));
    expect(list).toHaveFocus();
    expect(within(details).queryByRole("button", { name: "Inspect failure" })).toBeNull();
    store.dispose();
  });

  it("does not pause the next message merely because Dismiss focused the list toggle", () => {
    vi.useFakeTimers({ toFake: ["Date", "setTimeout", "clearTimeout"] });
    const store = new NotificationStore();
    const input = { source: "Settings", summary: "First", severity: "success" as const, lifetime: { kind: "transient" as const } };
    store.publish("first", "app", 1, input);
    store.publish("second", "app", 2, { ...input, summary: "Second" });
    render(<DesktopStatusBar store={store} route="settings" />);
    fireEvent.click(screen.getByRole("button", { name: "Dismiss notification" }));
    act(() => vi.advanceTimersByTime(5_000));
    expect(screen.getByText("Ready")).toBeVisible();
    store.dispose();
  });
});
