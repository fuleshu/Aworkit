// @vitest-environment jsdom
import { cleanup, renderHook } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import type { ReactNode } from "react";
import { NotificationProvider } from "../notifications/NotificationContext";
import { NotificationStore } from "../notifications/NotificationStore";
import type { RuntimeEvent } from "./corePort";
import { useCapabilityWarningNotices } from "./useCapabilityWarningNotices";

afterEach(cleanup);

function event(sequence: number, kind: string, payload: Record<string, unknown>): RuntimeEvent {
  return {
    schemaVersion: 1, streamId: "chat.1", branchId: "main", sequence,
    eventId: `event.${sequence}`, kind, payload,
  };
}
const warning = (sequence: number, message: string): RuntimeEvent =>
  event(sequence, "workflow.capability-warning", { message, status: "warning" });
const cleared = (sequence: number, message: string): RuntimeEvent =>
  event(sequence, "workflow.capability-warning-cleared", { message });

function render(
  store: NotificationStore,
  initialEvents: RuntimeEvent[],
) {
  const wrapper = ({ children }: { children: ReactNode }) => (
    <NotificationProvider store={store}>{children}</NotificationProvider>
  );
  return renderHook(
    (props: { events: RuntimeEvent[] }) =>
      useCapabilityWarningNotices(props.events, true, "chat.1"),
    { wrapper, initialProps: { events: initialEvents } },
  );
}

it("publishes one standing notice per distinct warning and resolves a cleared condition", () => {
  const store = new NotificationStore();
  const hook = render(store, []);
  // Live arrival: the first committed warning notifies.
  hook.rerender({ events: [warning(1, "mcp://comfyui.bridge/a")] });
  expect(store.getSnapshot().active).toHaveLength(1);
  expect(store.getSnapshot().active[0]).toMatchObject({
    summary: "mcp://comfyui.bridge/a",
    severity: "warning",
    lifetime: { kind: "condition", conditionId: "capability-warning:mcp://comfyui.bridge/a" },
  });
  // A second distinct warning is not suppressed by the first one's condition.
  hook.rerender({ events: [warning(1, "mcp://comfyui.bridge/a"), warning(2, "mcp://comfyui.bridge/b")] });
  expect(store.getSnapshot().active.map(item => item.summary)).toEqual([
    "mcp://comfyui.bridge/a", "mcp://comfyui.bridge/b",
  ]);
  // Re-rendering the same history never duplicates or reopens a notice.
  hook.rerender({ events: [warning(1, "mcp://comfyui.bridge/a"), warning(2, "mcp://comfyui.bridge/b")] });
  expect(store.getSnapshot().active).toHaveLength(2);
  // Clearing a condition removes only its notice.
  hook.rerender({
    events: [
      warning(1, "mcp://comfyui.bridge/a"),
      warning(2, "mcp://comfyui.bridge/b"),
      cleared(3, "mcp://comfyui.bridge/a"),
    ],
  });
  expect(store.getSnapshot().active.map(item => item.summary)).toEqual(["mcp://comfyui.bridge/b"]);
  store.dispose();
});

it("never reopens a warning when the committed history is reloaded", () => {
  const history = [warning(1, "mcp://comfyui.bridge/a")];
  const live = new NotificationStore();
  const arrived = render(live, []);
  arrived.rerender({ events: history });
  expect(live.getSnapshot().active).toHaveLength(1);
  arrived.unmount();
  live.dispose();

  // A reload presents the committed history on the first observation.
  const reloadedStore = new NotificationStore();
  const reloaded = render(reloadedStore, history);
  expect(reloadedStore.getSnapshot().active).toHaveLength(0);
  reloadedStore.dispose();
});
