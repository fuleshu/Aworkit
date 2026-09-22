// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { useChatRuntime } from "./useChatRuntime";
import type { ChatCorePort, RuntimeEvent, RuntimeSnapshot } from "./corePort";
import {
  runtimeEvent,
  chatSnapshot,
} from "../test/fixtures/chat";

afterEach(cleanup);
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(done => { resolve = done; });
  return { promise, resolve };
}
function event(id: string, sequence: number): RuntimeEvent {
  return runtimeEvent(sequence, "message.user", { body: id }, { streamId: id, eventId: `${id}.${sequence}` });
}
function snapshot(id: string, head = 1): RuntimeSnapshot {
  return chatSnapshot({
    chat: { chatId: id, runId: id, title: id, workflowId: "workflow.test", workflowName: "Test", lockedWorkflow: true },
    events: Array.from({ length: head }, (_, n) => event(id, n + 1)),
  });
}

describe("independent Chat projections", () => {
  it("keeps two pending commands separate and ignores foreign events and late completion", async () => {
    let selected = "chat.a", receive!: (event: RuntimeEvent) => void;
    const a = deferred<void>(), b = deferred<void>();
    const calls: string[] = [];
    const heads = new Map([["chat.a", 1], ["chat.b", 1]]);
    const port: ChatCorePort = {
      async snapshot(_after, id = selected) { return snapshot(id, heads.get(id)); },
      async subscribeEvents(listener) { receive = listener; return () => {}; },
      async command(intent) {
        if (intent.type === "select_chat") selected = intent.targetId;
        else {
          calls.push(intent.targetId!);
          await (intent.targetId === "chat.a" ? a.promise : b.promise);
          heads.set(intent.targetId!, 2);
        }
        return { accepted: true, commandId: intent.commandId, currentVersion: 2, reason: null };
      },
    };
    const { result } = renderHook(() => useChatRuntime(port, 60_000));
    await waitFor(() => expect(result.current.snapshot?.chat.chatId).toBe("chat.a"));
    let first!: Promise<boolean>, second!: Promise<boolean>;
    act(() => { first = result.current.dispatch({ type: "enqueue", commandId: "a.run", input: "A" }); });
    await waitFor(() => expect(calls).toEqual(["chat.a"]));
    await act(async () => { await result.current.dispatch({ type: "select_chat", targetId: "chat.b", commandId: "select.b" }); });
    expect(result.current.pendingCommandIds.size).toBe(0);
    act(() => { second = result.current.dispatch({ type: "enqueue", commandId: "b.run", input: "B" }); });
    await waitFor(() => expect(calls).toEqual(["chat.a", "chat.b"]));
    act(() => receive(event("chat.a", 2)));
    expect(result.current.events.every(e => e.streamId === "chat.b")).toBe(true);
    await act(async () => { a.resolve(); await first; });
    expect(result.current.snapshot?.chat.chatId).toBe("chat.b");
    expect([...result.current.pendingCommandIds]).toEqual(["b.run"]);
    await act(async () => { await result.current.dispatch({ type: "select_chat", targetId: "chat.a", commandId: "select.a" }); });
    expect(result.current.events).toHaveLength(2);
    await act(async () => { b.resolve(); await second; });
    expect(result.current.snapshot?.chat.chatId).toBe("chat.a");
    expect(result.current.events.every(e => e.streamId === "chat.a")).toBe(true);
  });

  it("buffers identical sequence numbers from different streams independently during startup", async () => {
    const initial = deferred<RuntimeSnapshot>();
    let receive!: (event: RuntimeEvent) => void;
    const port: ChatCorePort = {
      snapshot: () => initial.promise,
      async subscribeEvents(listener) { receive = listener; return () => {}; },
      async command() { throw new Error("unused"); },
    };
    const { result } = renderHook(() => useChatRuntime(port, 60_000));
    await act(async () => {
      receive(event("chat.a", 1)); receive(event("chat.b", 1));
      initial.resolve(snapshot("chat.a"));
    });
    expect(result.current.stale).toBe(false);
    expect(result.current.events.map(e => e.streamId)).toEqual(["chat.a"]);
  });

  it("keeps live events and ignores a snapshot overtaken by a newer response", async () => {
    const older = deferred<RuntimeSnapshot>(), newer = deferred<RuntimeSnapshot>();
    let reads = 0, receive!: (event: RuntimeEvent) => void;
    const port: ChatCorePort = {
      snapshot: () => ++reads === 1 ? Promise.resolve(snapshot("chat.a")) : reads === 2 ? older.promise : newer.promise,
      async subscribeEvents(listener) { receive = listener; return () => {}; },
      async command() { throw new Error("unused"); },
    };
    const { result } = renderHook(() => useChatRuntime(port, 60_000));
    await waitFor(() => expect(result.current.snapshot?.version).toBe(1));
    let first!: Promise<boolean>, second!: Promise<boolean>;
    act(() => { first = result.current.resynchronize(); second = result.current.resynchronize(); receive(event("chat.a", 2)); receive(event("chat.a", 3)); });
    await act(async () => { newer.resolve(snapshot("chat.a", 2)); await second; });
    expect(result.current.events).toHaveLength(3);
    await act(async () => { older.resolve(snapshot("chat.a", 1)); await first; });
    expect(result.current.snapshot?.version).toBe(2);
    expect(result.current.events).toHaveLength(3);
    expect(result.current.stale).toBe(false);
  });

  it("keeps maintenance queues and failed retries in their original Chat", async () => {
    const compact = deferred<void>();
    let selected = "chat.a", failFirst = true;
    const calls: string[] = [];
    const heads = new Map([["chat.a", 1], ["chat.b", 1]]);
    const port: ChatCorePort = {
      async snapshot(_after, id = selected) { return snapshot(id, heads.get(id)); },
      async command(intent, version) {
        if (intent.type === "select_chat") selected = intent.targetId;
        else {
          calls.push(`${intent.targetId}:${intent.commandId}:${version}`);
          if (intent.type === "compact_context") await compact.promise;
          if (intent.commandId === "first" && failFirst) { failFirst = false; throw new Error("retry input"); }
          heads.set(intent.targetId!, version + 1);
        }
        return { accepted: true, commandId: intent.commandId, currentVersion: version + 1, reason: null };
      },
    };
    const { result } = renderHook(() => useChatRuntime(port, 60_000));
    await waitFor(() => expect(result.current.snapshot?.chat.chatId).toBe("chat.a"));
    let maintenance!: Promise<boolean>;
    act(() => { maintenance = result.current.dispatch({ type: "compact_context", commandId: "compact", nodeId: "agent", baseSequence: 1, targetId: "chat.a" }); });
    await act(async () => { await result.current.dispatch({ type: "enqueue", commandId: "first", input: "First" }); });
    await act(async () => { await result.current.dispatch({ type: "select_chat", commandId: "select.b", targetId: "chat.b" }); });
    expect(result.current.maintenancePending).toBe(false);
    expect(result.current.queuedMaintenanceInputs).toEqual([]);
    await act(async () => { await result.current.dispatch({ type: "enqueue", commandId: "other", input: "B" }); });
    await act(async () => { compact.resolve(); await maintenance; });
    expect(result.current.error).toBeNull();
    await act(async () => { await result.current.dispatch({ type: "select_chat", commandId: "select.a", targetId: "chat.a" }); });
    expect(result.current.queuedMaintenanceInputs).toEqual(["First"]);
    await act(async () => { await result.current.dispatch({ type: "enqueue", commandId: "second", input: "Second" }); });
    expect(calls).toEqual(["chat.a:compact:1", "chat.b:other:1", "chat.a:first:2", "chat.a:first:2", "chat.a:second:3"]);
    expect(result.current.queuedMaintenanceInputs).toEqual([]);
  });
});
