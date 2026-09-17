// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { useChatRuntime } from "./useChatRuntime";
import type { ChatCorePort, ChatEventPage, RuntimeEvent, RuntimeSnapshot } from "./corePort";
afterEach(cleanup);
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(done => { resolve = done; });
  return { promise, resolve };
}
function events(id: string, first: number, last: number): RuntimeEvent[] {
  return Array.from({ length: last - first + 1 }, (_, n) => ({ schemaVersion: 1, streamId: id, branchId: "main", sequence: first+n, eventId: `${id}.${first+n}`, kind: "message.user", payload: { body: String(first+n) } }));
}
function snapshot(id: string, first = 101, last = 110): RuntimeSnapshot {
  return { version:last, throughSequence:last, reducerVersion:"test", stateHash:"sha256:test",
    chat: { chatId:id, runId:id, title:id, scope:"No project", workflowId:null, workflowName:null, branch:null, projectId:null, phase:"waiting_input", lockedWorkflow:false, recoveryPending:false, queuedInputs:[], expectedVersion:last },
    history:["a", "b", "c"].map(chatId => ({ chatId, runId:chatId, parentChatId:null, title:chatId, projectId:null, projectName:null, phase:"waiting_input" as const, pinned:false, createdAt:"1", updatedAt:"1" })),
    projects:[], evidence:[], events:events(id,first,last),
    eventWindow:{ firstSequence:first, lastSequence:last, headSequence:last, hasMore:first>1, supportingEvents:[] },
  };
}
it("prepends contiguous older pages once and keeps the live tail", async () => {
  const older = deferred<ChatEventPage>();
  let receive!: (event: RuntimeEvent) => void, calls = 0;
  const port: ChatCorePort = {
    async snapshot() { return snapshot("a"); },
    async command() { throw Error("unused"); },
    async subscribeEvents(listener) { receive=listener; return () => {}; },
    async olderEvents(id, before) { calls++; expect(id).toBe("a"); expect(before).toBe(101); return older.promise; },
  };
  const {result} = renderHook(() => useChatRuntime(port,60000));
  await waitFor(() => expect(result.current.firstSequence).toBe(101));
  expect(result.current.stale).toBe(false);
  let request!: Promise<void>;
  act(() => { request=result.current.loadOlder(); void result.current.loadOlder(); receive(events("a",111,111)[0]); });
  expect(calls).toBe(1); expect(result.current.olderLoading).toBe(true);
  await act(async () => { older.resolve({events:events("a",91,100),window:{firstSequence:91,lastSequence:100,headSequence:110,hasMore:true,supportingEvents:[]}}); await request; });
  expect(result.current.events.map(e=>e.sequence)).toEqual(Array.from({length:21},(_,i)=>91+i));
  expect(result.current.firstSequence).toBe(91);
});
it("selects immediately, loads another Chat concurrently, and ignores obsolete reads", async () => {
  const b = deferred<RuntimeSnapshot>(), older = deferred<ChatEventPage>();
  let selected="a", readingB=false;
  const port: ChatCorePort = {
    async snapshot(_after,id=selected) { if(id==="b") {readingB=true;return b.promise;} return snapshot(id); },
    async olderEvents() { return older.promise; },
    async command(intent) { if(intent.type==="select_chat") selected=intent.targetId; return {accepted:true,commandId:intent.commandId,currentVersion:110,reason:null}; },
  };
  const {result} = renderHook(() => useChatRuntime(port,60000));
  await waitFor(() => expect(result.current.firstSequence).toBe(101));
  let old!:Promise<void>, first!:Promise<boolean>;
  act(() => { old=result.current.loadOlder(); first=result.current.dispatch({type:"select_chat",targetId:"b",commandId:"b.select"}); });
  expect(result.current.snapshot?.chat.chatId).toBe("b"); expect(result.current.loading).toBe(true); expect(result.current.events).toEqual([]);
  await waitFor(() => expect(readingB).toBe(true));
  await act(async () => { await result.current.dispatch({type:"select_chat",targetId:"c",commandId:"c.select"}); });
  expect(result.current.snapshot?.chat.chatId).toBe("c"); expect(result.current.loading).toBe(false);
  await act(async () => { b.resolve(snapshot("b")); older.resolve({events:events("a",91,100),window:{firstSequence:91,lastSequence:100,headSequence:110,hasMore:true,supportingEvents:[]}}); await Promise.all([first,old]); });
  expect(result.current.snapshot?.chat.chatId).toBe("c"); expect(result.current.events.every(e=>e.streamId==="c")).toBe(true);
});
it("rejects gaps inside the loaded range without requiring the omitted prefix", async () => {
  const next=snapshot("a");
  const port:ChatCorePort={ async snapshot(){return {...next,events:next.events.filter(e=>e.sequence!==105)};},async command(){throw Error("unused");} };
  const {result}=renderHook(()=>useChatRuntime(port,60000));
  await waitFor(()=>expect(result.current.stale).toBe(true));
  expect(result.current.error?.message).toContain("gap");
});

it("continues past raw pages with no earlier visible activity", async () => {
  const next = snapshot("a", 101, 110);
  let calls = 0;
  const invisible = (first:number,last:number) => events("a",first,last).map(event=>({...event,kind:"context.checkpoint"}));
  const port:ChatCorePort={
    async snapshot(){return next;}, async command(){throw Error("unused");},
    async olderEvents(_id,before) {
      calls++;
      const first=before-10;
      return {events:calls===1?invisible(first,before-1):events("a",first,before-1),window:{firstSequence:first,lastSequence:before-1,headSequence:110,hasMore:true,supportingEvents:[]}};
    },
  };
  const {result}=renderHook(()=>useChatRuntime(port,60000));
  await waitFor(()=>expect(result.current.firstSequence).toBe(101));
  await act(async()=>{await result.current.loadOlder();});
  expect(calls).toBe(2);
  expect(result.current.firstSequence).toBe(81);
  expect(result.current.events.filter(event=>event.sequence>=101)).toEqual(next.events);
});
