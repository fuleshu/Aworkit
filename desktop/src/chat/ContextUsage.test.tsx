// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeAll, expect, it, vi } from "vitest";
import { ContextUsage } from "./ContextUsage";
import { CompressionUsage } from "./CompressionUsage";
import { contextModel, contextUsage, estimateContext, projectContexts, type ContextDocument, type ContextSelection } from "./contextProjection";
import { projectSemanticTimeline } from "./activityProjection";
import type { RuntimeEvent } from "./corePort";
import { runtimeEvent } from "../test/fixtures/chat";

beforeAll(() => { HTMLDialogElement.prototype.showModal = function() { this.setAttribute("open", ""); }; });
afterEach(cleanup);
const event = (sequence: number, kind: string, payload: Record<string, unknown>): RuntimeEvent =>
  runtimeEvent(sequence, kind, payload, { streamId: "chat.context", eventId: `e.${sequence}` });
const events = [
  event(1, "span.started", { spanId: "node", spanKind: "graph_node", nodeId: "agent.1", label: "Agent" }),
  event(2, "span.started", { spanId: "loop", spanKind: "agent_loop", parentSpanId: "node" }),
  event(3, "span.started", { spanId: "model", spanKind: "model_call", parentSpanId: "loop", input: { messages: [{ role: "system", content: "Original system" }, { role: "user", content: "Question" }] } }),
  event(4, "span.usage", { spanId: "model", inputTokens: 800, outputTokens: 100 }),
  event(5, "span.completed", { spanId: "model", output: [{ kind: "assistant_output", text: "The answer" }] }),
];
it("fills an older Chat's missing capacity only from the same model", () => {
  const old = [event(1, "span.started", { contextModel: { name: "Fixture", contextWindow: null } })];
  expect(contextModel(old, { name: "Fixture", contextWindow: 32768 })?.contextWindow).toBe(32768);
  expect(contextModel(old, { name: "Different", contextWindow: 32768 })?.contextWindow).toBeNull();
});
it("reports local compression separately and excludes other nodes and children",()=>{
 render(<CompressionUsage nodeId="agent.1" events={[
  event(1,"context.compression",{nodeId:"agent.1",child:null,metrics:{beforeBytes:10000,afterBytes:2000}}),
  event(2,"context.compression",{nodeId:"agent.2",child:null,metrics:{beforeBytes:10000,afterBytes:0}}),
  event(3,"context.retrieved",{nodeId:"agent.1",child:null}),
  event(4,"context.retrieved",{nodeId:"agent.1",child:"private"}),
 ]}/>);
 expect(screen.getByText(/8K bytes saved across 1 result · 1 retrieval/)).toBeTruthy();
});
const open = () => {
  fireEvent.click(screen.getByRole("button", { name: /Context usage/ }));
  fireEvent.click(screen.getByRole("button", { name: "Display Context" }));
};

it("projects current context, final answer and per-request usage, excluding child agents", () => {
  const all = [...events,
    event(6, "span.started", { spanId: "child", spanKind: "external_agent", parentSpanId: "loop" }),
    event(7, "span.started", { spanId: "child-model", spanKind: "model_call", parentSpanId: "child", input: { messages: [{ role: "user", content: "private child" }] } }),
  ];
  const [context] = projectContexts(all);
  expect(projectContexts(all)).toHaveLength(1);
  expect(context.sequence).toBe(5);
  expect(context.inputTokens).toBe(800);
  expect(documentOf(context).contextMessages[0]).toEqual({ afterExchanges: 0, role: "assistant", content: "The answer" });
  expect(estimateContext(documentOf(context)).total).toBeGreaterThan(0);
});

it("opens read only, unlocks editing, and saves on the close control", async () => {
  const save = vi.fn().mockResolvedValue(true);
  render(<ContextUsage events={events} model={{ name: "Fixture", contextWindow: 32000 }} editDisabledReason={null} onSave={save} />);
  open();
  const editor = screen.getByRole("textbox", { name: "Raw model context" });
  expect(editor).toHaveProperty("readOnly", true);
  fireEvent.click(screen.getByRole("button", { name: "Enable Edit" }));
  expect(editor).toHaveProperty("readOnly", false);
  fireEvent.change(editor, { target: { value: (editor as HTMLTextAreaElement).value.replace("Original system", "Edited system") } });
  fireEvent.click(screen.getByRole("button", { name: "Close context" }));
  await waitFor(() => expect(save).toHaveBeenCalledTimes(1));
  expect(save.mock.calls[0][1].input.messages[0].content).toBe("Edited system");
  await waitFor(() => expect(screen.queryByRole("textbox")).toBeNull());
});

it("keeps invalid and rejected edits open, including Escape, and allows explicit discard", async () => {
  const save = vi.fn().mockResolvedValue(false);
  render(<ContextUsage events={events} editDisabledReason={null} onSave={save} />);
  open();
  fireEvent.click(screen.getByRole("button", { name: "Enable Edit" }));
  const editor = screen.getByRole("textbox");
  const original = (editor as HTMLTextAreaElement).value;
  fireEvent.change(editor, { target: { value: "{" } });
  fireEvent(screen.getByRole("dialog", { name: "Model context" }), new Event("cancel", { bubbles: false, cancelable: true }));
  expect(screen.getByRole("alert")).toBeTruthy();
  expect(save).not.toHaveBeenCalled();
  fireEvent.change(editor, { target: { value: original.replace("Question", "Revised") } });
  fireEvent.click(screen.getByRole("button", { name: "Save and Close" }));
  await waitFor(() => expect(save).toHaveBeenCalledTimes(1));
  expect(screen.getByRole("textbox")).toHaveProperty("value", original.replace("Question", "Revised"));
  fireEvent.click(screen.getByRole("button", { name: "Discard changes" }));
  expect(screen.queryByRole("textbox")).toBeNull();
});

it("locks active context, closes unchanged without saving, and projects an edit event", () => {
  const save = vi.fn();
  render(<ContextUsage events={events} editDisabledReason="Finish the current turn." onSave={save} />);
  open();
  expect(screen.getByRole("button", { name: "Enable Edit" })).toHaveProperty("disabled", true);
  fireEvent.click(screen.getByRole("button", { name: "Close context" }));
  expect(save).not.toHaveBeenCalled();
  const document = documentOf(projectContexts(events)[0]);
  const edited = event(6, "context.edited", { nodeId: "agent.1", document, body: "User edited context." });
  expect(projectContexts([...events, edited])[0]).toMatchObject({ sequence: 6, edited: true, inputTokens: null });
  expect(projectSemanticTimeline([edited])[0].title).toBe("Context edited");
});

it("shows an honest empty state when model context or capacity is unavailable", () => {
  render(<ContextUsage events={[]} editDisabledReason={null} onSave={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: /Context usage/ }));
  expect(screen.getByText(/Context is available after the first model call/)).toBeTruthy();
  expect(screen.getByRole("button", { name: "Display Context" })).toHaveProperty("disabled", true);
});

it("uses actual provider tokens even when the character estimate is larger",()=>{
  const selected=projectContexts(events)[0];
  const large={...selected,inputTokens:1,outputTokens:1,document:{...documentOf(selected),input:{messages:[{role:"user" as const,content:"large ".repeat(1000)}]}}};
  expect(contextUsage(large).reported).toBe(true);
  expect(contextUsage(large).total).toBe(2);
});

it("compacts only the selected idle context and displays the committed pressure",async()=>{
  const compact=vi.fn().mockResolvedValue(true);
  const checkpoint=event(6,"context.checkpoint",{nodeId:"agent.1",child:null,pressureTokens:120,pressureReported:true,snapshot:{document:{...documentOf(projectContexts(events)[0]),input:{messages:[{role:"user",content:"<compacted-summary>checkpoint</compacted-summary>"}]}}}});
  const selected=projectContexts([...events,checkpoint])[0];
  expect(contextUsage(selected).total).toBe(120);
  const view=render(<ContextUsage events={[...events,checkpoint]} editDisabledReason={null} onSave={vi.fn()} onCompact={compact}/>);
  fireEvent.click(screen.getByRole("button",{name:/Context usage/}));
  fireEvent.click(screen.getByRole("button",{name:"Compact context"}));
  await waitFor(()=>expect(compact).toHaveBeenCalledWith(selected));
  view.rerender(<ContextUsage events={events} editDisabledReason="Finish the current turn." onSave={vi.fn()} onCompact={compact}/>);
  fireEvent.click(screen.getByRole("button",{name:/Context usage/}));
  expect(screen.getByRole("button",{name:"Compact context"})).toHaveProperty("disabled",true);
});

it("settles one compaction activity card and exposes summary failures without Chat messages",()=>{
  const lifecycle=[event(1,"context.compaction-started",{compactionId:"c1"}),event(2,"context.compacted",{compactionId:"c1",strategy:"summary"}),event(3,"context.compaction-ended",{compactionId:"c1",error:null})];
  const timeline=projectSemanticTimeline(lifecycle);
  expect(timeline).toHaveLength(1);expect(timeline[0].status).toBe("completed");
  const failure=projectSemanticTimeline([lifecycle[0],event(2,"context.compaction-ended",{compactionId:"c1",error:"Summary was truncated"})]);
  expect(failure[0]).toMatchObject({status:"failed",body:"Summary was truncated"});
});

it("states a released checkpoint instead of reporting no context at all",()=>{
  const checkpoint=event(6,"context.checkpoint",{nodeId:"agent.1",child:null,pressureTokens:120,pressureReported:true,prunedPayload:marker("context_checkpoint",2_900_000)});
  const selected=projectContexts([...events,checkpoint])[0];
  expect(selected).toMatchObject({
    nodeId:"agent.1", sequence:6, document:null, pressureTokens:120, pressureReported:true,
    released:{ kind:"context_checkpoint", bytes:2_900_000, digestBefore:`sha256:${"a".repeat(64)}`, prunedAt:"2026-08-03 14:02:11" },
  });
  // The reported pressure survives the release; only the breakdown is unknown.
  expect(contextUsage(selected)).toMatchObject({ available:false, known:true, total:120, reported:true });
  render(<ContextUsage events={[...events,checkpoint]} model={{name:"Fixture",contextWindow:32000}} editDisabledReason={null} onSave={vi.fn()} onCompact={vi.fn()} />);
  fireEvent.click(screen.getByRole("button",{name:/Context usage/}));
  const notice=screen.getByRole("note");
  expect(notice.textContent).toContain("Context snapshot released — this turn's snapshot was superseded by newer turns, so its 2.9 MB context snapshot was released to bound this Chat's store size. The newest snapshot of every context scope and the latest 20 turns are kept.");
  expect(notice.textContent).toContain("Released 2026-08-03 14:02:11");
  expect(notice.getAttribute("title")).toContain(`Canonical digest before release: sha256:${"a".repeat(64)}`);
  expect(screen.queryByText("System prompt")).toBeNull();
  // The omission stays inspectable rather than opening an empty or invalid editor.
  expect(screen.getByRole("button",{name:"Compact context"})).toHaveProperty("disabled",true);
  fireEvent.click(screen.getByRole("button",{name:"Display Context"}));
  expect(screen.queryByRole("textbox")).toBeNull();
  expect(screen.getByRole("dialog",{name:"Model context"}).textContent).toContain("Context snapshot released");
});

it("states a released request body without losing the call's usage",()=>{
  const released=[
    event(1,"span.started",{spanId:"node",spanKind:"graph_node",nodeId:"agent.1",label:"Agent"}),
    event(2,"span.started",{spanId:"loop",spanKind:"agent_loop",parentSpanId:"node"}),
    event(3,"span.started",{spanId:"model",spanKind:"model_call",parentSpanId:"loop",hasInput:true,prunedPayload:marker("model_call_input",1_200_000)}),
    event(4,"span.usage",{spanId:"model",inputTokens:800,outputTokens:100}),
    event(5,"span.completed",{spanId:"model",output:[{kind:"assistant_output",text:"The answer"}]}),
  ];
  const selected=projectContexts(released)[0];
  expect(selected).toMatchObject({ nodeId:"agent.1", sequence:3, document:null, released:{ kind:"model_call_input", bytes:1_200_000 } });
  expect(selected.inputTokens).toBe(800);
  expect(contextUsage(selected)).toMatchObject({ available:false, known:true, total:900, reported:true });
  render(<ContextUsage events={released} editDisabledReason={null} onSave={vi.fn()} />);
  fireEvent.click(screen.getByRole("button",{name:/Context usage/}));
  const notice=screen.getByRole("note");
  expect(notice.textContent).toContain("Request body released — this turn's snapshot was superseded by newer turns, so its 1.2 MB request body was released to bound this Chat's store size. Its usage, timing and result are unaffected.");
  expect(screen.getByText(/Last request reported 800 input \/ 100 output tokens/)).toBeTruthy();
  expect(screen.getByRole("button",{name:"Display Context"})).toHaveProperty("disabled",false);
});

it("keeps an intact request body readable and a released one out of the breakdown",()=>{
  const intact=projectContexts(events)[0];
  expect(intact.released).toBeUndefined();
  expect(documentOf(intact).input.messages).toHaveLength(2);
  expect(contextUsage(intact)).toMatchObject({ available:true, known:true });
});

/** A released payload's marker: the heavy field is gone and this replaces it. */
function marker(kind:"context_checkpoint"|"model_call_input",bytes:number):Record<string,unknown>{
  return { schemaVersion:1, kind, bytes, digestBefore:`sha256:${"a".repeat(64)}`, prunedAt:"2026-08-03 14:02:11", retainedTurns:20, reason:"Superseded by newer turns." };
}

/** An intact fixture must project a readable document; a release would null it. */
function documentOf(selection:ContextSelection):ContextDocument{
  if(selection.document===null)throw new Error(`expected a readable context document for ${selection.nodeId}`);
  return selection.document;
}
