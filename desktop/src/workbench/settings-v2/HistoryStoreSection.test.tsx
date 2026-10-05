// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { HistoryStoreSection } from "./HistoryStoreSection";
import { TauriSettingsV2CorePort } from "../settingsV2Port";

const native = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
  unlisten: vi.fn(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: native.listen }));

beforeEach(() => {
  native.invoke.mockReset();
  native.listen.mockReset();
  native.unlisten.mockReset();
  native.listen.mockResolvedValue(native.unlisten);
});

afterEach(cleanup);

/** The measured projection the trusted core returns for a 30.8 GB store. */
function historyStoreProjection(
  outbox: {
    readonly rows: number;
    readonly deliveredRows: number;
    readonly bytes: number;
  } = { rows: 573_690, deliveredRows: 573_178, bytes: 16_277_250_048 },
): Record<string, unknown> {
  return {
    storeBytes: 33_000_000_000,
    payloadBytes: 16_000_000_000,
    snapshotBytes: 15_000_000_000,
    deletedChats: 29,
    deletedChatBytes: 900_000_000,
    outboxRows: outbox.rows,
    outboxDeliveredRows: outbox.deliveredRows,
    outboxBytes: outbox.bytes,
    retainedTurns: 20,
    kinds: [
      { kind: "context.checkpoint", events: 7_694, bytes: 7_500_000_000 },
      { kind: "span.started", events: 7_923, bytes: 7_500_000_000 },
      { kind: "span.content_delta", events: 482_155, bytes: 197_000_000 },
    ],
  };
}

function historyReclaimProjection(
  outbox: {
    readonly rowsRemoved: number;
    readonly bytesReleased: number;
  } = { rowsRemoved: 573_178, bytesReleased: 16_277_250_048 },
): Record<string, unknown> {
  return {
    report: {
      streamsScanned: 124,
      payloadsPruned: 15_383,
      payloadBytesReleased: 8_200_000_000,
      chatsPurged: 29,
      eventsRemoved: 52_118,
      outboxRowsRemoved: outbox.rowsRemoved,
      outboxBytesReleased: outbox.bytesReleased,
    },
    storeBytesBefore: 33_000_000_000,
    storeBytesAfter: 21_000_000_000,
  };
}

function deferred<T>(): {
  readonly promise: Promise<T>;
  readonly resolve: (value: T) => void;
} {
  let settle: ((value: T) => void) | undefined;
  const promise = new Promise<T>((resolve) => {
    settle = resolve;
  });
  return { promise, resolve: (value) => settle?.(value) };
}

function commandCalls(command: string): number {
  return native.invoke.mock.calls.filter(([name]) => name === command).length;
}

/** The section is driven by the real port, so the mocks are the native edge. */
function renderSection(
  options: {
    readonly confirm?: (title: string, body: string) => Promise<boolean>;
  } = {},
): TauriSettingsV2CorePort {
  const port = new TauriSettingsV2CorePort();
  render(
    <HistoryStoreSection
      loadStatus={() => port.historyStoreStatus()}
      reclaim={() => port.historyReclaim()}
      onProgress={(handler) => port.onHistoryReclaimProgress(handler)}
      confirm={options.confirm}
    />,
  );
  return port;
}

describe("History store panel", () => {
  it("states the measured store, the reclaimable pool and the retention floor", async () => {
    native.invoke.mockImplementation(async (command: string) => {
      if (command === "desktop_history_store_status")
        return historyStoreProjection();
      throw new Error(`unexpected command: ${command}`);
    });
    renderSection();

    expect(await screen.findByText("30.7 GB")).toBeVisible();
    expect(screen.getByText("14.9 GB")).toBeVisible();
    expect(screen.getByText("14.0 GB")).toBeVisible();
    expect(screen.getByText("29 holding 858.3 MB")).toBeVisible();
    expect(
      screen.getByText(
        /always keeps the newest snapshot of every context scope and its newest 20 turns/,
      ),
    ).toBeVisible();
    // The measured breakdown is the reason the store stops being invisible.
    expect(screen.getByText("context.checkpoint")).toBeVisible();
    expect(screen.getByText("7,694")).toBeVisible();
    expect(screen.getByText("span.content_delta")).toBeVisible();
    expect(
      screen.getByText(/only superseded context snapshots and superseded model-call request bodies/i),
    ).toBeVisible();
    // The delivery queue's second copy of every delivered event is measured
    // where every other cost is, instead of hiding half the store.
    expect(screen.getByText("Delivery records still queued")).toBeVisible();
    expect(
      screen.getByText("573,690 holding 15.2 GB; 573,178 already delivered"),
    ).toBeVisible();
    expect(
      screen.getByText(/keeps a copy of every event it has already delivered/),
    ).toBeVisible();
  });

  it("adds no delivery row when the delivery queue holds nothing", async () => {
    native.invoke.mockImplementation(async (command: string) => {
      if (command === "desktop_history_store_status")
        return historyStoreProjection({ rows: 0, deliveredRows: 0, bytes: 0 });
      throw new Error(`unexpected command: ${command}`);
    });
    renderSection();

    await screen.findByText("30.7 GB");
    expect(screen.queryByText("Delivery records still queued")).toBeNull();
    expect(
      screen.queryByText(/keeps a copy of every event it has already delivered/),
    ).toBeNull();
    // An empty queue silences only its own line.
    expect(screen.getByText("Deleted Chats still stored")).toBeVisible();
  });

  it("asks before it releases anything, then reclaims on confirmation", async () => {
    native.invoke.mockImplementation(async (command: string) => {
      if (command === "desktop_history_store_status")
        return historyStoreProjection();
      if (command === "desktop_history_reclaim")
        return historyReclaimProjection();
      throw new Error(`unexpected command: ${command}`);
    });
    renderSection();

    await screen.findByText("30.7 GB");
    fireEvent.click(screen.getByRole("button", { name: "Reclaim space…" }));

    expect(
      screen.getByRole("heading", { name: "Reclaim history store space?" }),
    ).toBeVisible();
    expect(screen.getByText(/will release at most 30.0 GB/)).toBeVisible();
    expect(
      screen.getByText(/drop the delivered copies its delivery queue still holds/),
    ).toBeVisible();
    expect(
      screen.getByText(/cannot do other work until it finishes/),
    ).toBeVisible();
    expect(commandCalls("desktop_history_reclaim")).toBe(0);

    fireEvent.click(screen.getByRole("button", { name: "Reclaim space" }));
    await waitFor(() =>
      expect(commandCalls("desktop_history_reclaim")).toBe(1),
    );
    expect(native.invoke).toHaveBeenCalledWith("desktop_history_reclaim");
    // The measured line is read again, because the pass changed the store.
    await waitFor(() =>
      expect(commandCalls("desktop_history_store_status")).toBe(2),
    );
  });

  it("keeps everything when the app's own confirmation is declined", async () => {
    native.invoke.mockImplementation(async (command: string) => {
      if (command === "desktop_history_store_status")
        return historyStoreProjection();
      throw new Error(`unexpected command: ${command}`);
    });
    const confirm = vi.fn().mockResolvedValue(false);
    renderSection({ confirm });

    await screen.findByText("30.7 GB");
    fireEvent.click(screen.getByRole("button", { name: "Reclaim space…" }));

    await waitFor(() => expect(confirm).toHaveBeenCalledOnce());
    expect(confirm.mock.calls[0]![0]).toBe("Reclaim history store space?");
    expect(confirm.mock.calls[0]![1]).toContain("at most 30.0 GB");
    // The confirmation names the delivery duplicates it removes, so the number
    // it states is not understated.
    expect(confirm.mock.calls[0]![1]).toContain(
      "drop the delivered copies its delivery queue still holds",
    );
    expect(commandCalls("desktop_history_reclaim")).toBe(0);
  });

  it("shows the pass's phase and progress, then the store size it moved", async () => {
    const pass = deferred<unknown>();
    native.invoke.mockImplementation(async (command: string) => {
      if (command === "desktop_history_store_status")
        return historyStoreProjection();
      if (command === "desktop_history_reclaim") return pass.promise;
      throw new Error(`unexpected command: ${command}`);
    });
    renderSection({ confirm: async () => true });
    await screen.findByText("30.7 GB");
    await waitFor(() => expect(native.listen).toHaveBeenCalledWith(
      "aworkit:history-reclaim",
      expect.any(Function),
    ));
    const emit = native.listen.mock.calls[0]![1] as (event: {
      payload: unknown;
    }) => void;

    fireEvent.click(screen.getByRole("button", { name: "Reclaim space…" }));
    await waitFor(() =>
      expect(commandCalls("desktop_history_reclaim")).toBe(1),
    );
    expect(screen.getByRole("button", { name: "Reclaiming…" })).toBeDisabled();

    act(() => {
      emit({ payload: { phase: "releasing", done: 42, total: 153 } });
    });
    expect(
      screen.getByText("Releasing superseded snapshots — 42 of 153 Chats"),
    ).toBeVisible();

    act(() => {
      emit({ payload: { phase: "rewriting", done: 0, total: 0 } });
    });
    expect(
      screen.getByText("Rewriting the store file… this is the slow part"),
    ).toBeVisible();

    await act(async () => {
      pass.resolve(historyReclaimProjection());
    });
    expect(await screen.findByText(/Store size 30.7 GB → 19.6 GB/)).toBeVisible();
    expect(screen.getByText(/Released 7.6 GB/)).toBeVisible();
    // The pass removed the delivery duplicates as well, and says how much.
    expect(
      screen.getByText(
        "Removed 573,178 already-delivered records from the delivery queue.",
      ),
    ).toBeVisible();
    expect(
      screen.getByText("The removed delivery copies held 15.2 GB."),
    ).toBeVisible();
    expect(screen.getByRole("button", { name: "Reclaim space…" })).toBeEnabled();
  });

  it("says nothing about delivery duplicates when the pass removed none", async () => {
    native.invoke.mockImplementation(async (command: string) => {
      if (command === "desktop_history_store_status")
        return historyStoreProjection();
      if (command === "desktop_history_reclaim")
        return historyReclaimProjection({ rowsRemoved: 0, bytesReleased: 0 });
      throw new Error(`unexpected command: ${command}`);
    });
    renderSection({ confirm: async () => true });

    await screen.findByText("30.7 GB");
    fireEvent.click(screen.getByRole("button", { name: "Reclaim space…" }));

    expect(await screen.findByText(/Store size 30.7 GB → 19.6 GB/)).toBeVisible();
    expect(screen.queryByText(/from the delivery queue/)).toBeNull();
    expect(screen.queryByText(/removed delivery copies held/)).toBeNull();
  });

  it("reports the refusal instead of pretending the pass finished", async () => {
    native.invoke.mockImplementation(async (command: string) => {
      if (command === "desktop_history_store_status")
        return historyStoreProjection();
      if (command === "desktop_history_reclaim")
        throw new Error("the store refused the rewrite");
      throw new Error(`unexpected command: ${command}`);
    });
    renderSection({ confirm: async () => true });

    await screen.findByText("30.7 GB");
    fireEvent.click(screen.getByRole("button", { name: "Reclaim space…" }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(
      "The reclaim pass did not finish: the store refused the rewrite",
    );
    expect(screen.queryByText(/Store size/)).toBeNull();
    expect(screen.getByRole("button", { name: "Reclaim space…" })).toBeEnabled();
  });
});
