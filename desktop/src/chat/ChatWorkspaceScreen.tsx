import { useMemo, useRef, useState } from "react";
import { controlsFor, emptyComposer, submitIntent, updateComposer } from "./composer";
import { inspectEvidence, queryEvidence } from "./evidence";
import { toConversationCard, visibleTimeline } from "./conversation";
import { ChatWorkspaceController } from "./workspace";
import type { ChatIntent, EvidenceRecord, TimelineItem } from "./types";

const sampleTimeline: readonly TimelineItem[] = [
  { id: "welcome", kind: "message", title: "Aworkit", body: "Choose a workflow and send your first message.", createdAt: "now" },
  { id: "status", kind: "verification", title: "Projection ready", body: "The timeline is a core projection.", createdAt: "now", raw: { sequence: 0 } },
];
const sampleEvidence: readonly EvidenceRecord[] = [
  { id: "projection", category: "provenance", label: "Projection sequence", value: { sequence: 0, source: "local core" }, state: "available" },
  { id: "debug", category: "debug", label: "Detailed capture", value: null, state: "unsupported" },
];

function intentForCardAction(controller: ChatWorkspaceController, action: NonNullable<TimelineItem["action"]>, targetId: string): ChatIntent {
  if (action === "approve" || action === "reject") return { type: "approval", commandId: controller.createIntent("approval", targetId).commandId, targetId, approved: action === "approve" };
  return controller.createIntent(action, targetId);
}

/** Composes header, virtual timeline, composer, controls, and explicit evidence facts. */
export function ChatWorkspaceScreen(): React.JSX.Element {
  const controller = useMemo(() => new ChatWorkspaceController(), []);
  const [composer, setComposer] = useState(emptyComposer);
  const [timeline, setTimeline] = useState(sampleTimeline);
  const [queuedPreview, setQueuedPreview] = useState<readonly string[]>([]);
  const [selectedEvidence, setSelectedEvidence] = useState("projection");
  const [notice, setNotice] = useState<string | null>(null);
  const scrollAnchor = useRef<HTMLDivElement>(null);
  const chat = controller.snapshot().chat;
  const cards = visibleTimeline(timeline, Math.max(0, timeline.length - 40), 40).map(toConversationCard);
  const evidence = queryEvidence(sampleEvidence, { offset: 0, limit: 20 });
  const dispatch = (intent: ChatIntent) => {
    setNotice(`Queued ${intent.type} as ${intent.commandId}; draft remains until a core receipt.`);
    if (intent.type === "start" || intent.type === "enqueue") {
      setTimeline((items) => [...items, { id: intent.commandId, kind: "message", title: "You", body: intent.input, createdAt: "now" }]);
      setQueuedPreview((items) => [...items, intent.input]);
    }
  };
  const send = () => {
    try { dispatch(submitIntent(composer, chat, controller.createIntent("enqueue").commandId)); }
    catch (error) { setNotice(error instanceof Error ? error.message : "Unable to send input."); }
  };
  return <section className="chat-workspace" aria-label="Chat workspace">
    <header className="chat-header"><div><p className="eyebrow">{chat.scope} · {chat.phase}</p><h2>{chat.title}</h2><small>{chat.workflowName ?? "Workflow selected on first send"}{chat.lockedWorkflow ? " · workflow locked" : ""}</small></div><div className="run-controls" aria-label="Run controls">{controlsFor(chat).map((type) => <button key={type} title={`${type} this Run through the typed command gateway`} type="button" onClick={() => dispatch(controller.createIntent(type))}>{type}</button>)}</div></header>
    {controller.isStale() && <p className="projection-stale" role="status">Projection is stale. The last contiguous timeline is preserved while Aworkit resynchronizes.</p>}
    <div className="timeline" ref={scrollAnchor} aria-live="polite">{cards.map((card) => <article className={`timeline-card ${card.label.toLowerCase().replaceAll(" ", "-")}`} key={card.id}><small>{card.label}{card.reasoningLabel === undefined ? "" : ` · ${card.reasoningLabel}`}</small><p>{card.content}</p>{card.action !== undefined && <button title={`Send ${card.action} through the typed command gateway`} type="button" onClick={() => dispatch(intentForCardAction(controller, card.action!, card.id))}>{card.action}</button>}{card.inspectable && <button title="Select evidence for this timeline item" type="button" onClick={() => { controller.selectEvidence(card.id); setSelectedEvidence("projection"); }}>Inspect</button>}</article>)}</div>
    <div className="composer"><label>Workflow<select aria-label="Workflow for the first Chat input" title="The first sent input freezes this workflow for the Chat" value={composer.workflowId} disabled={chat.lockedWorkflow} onChange={(event) => setComposer((state) => updateComposer(state, { workflowId: event.target.value }))}><option value="starter">Starter workflow</option><option value="review">Review workflow</option></select></label><textarea aria-label="Chat input" placeholder="Message Aworkit" title="Enter a message; it is retained until the core confirms receipt" value={composer.draft} onCompositionStart={() => setComposer((state) => updateComposer(state, { imeComposing: true }))} onCompositionEnd={() => setComposer((state) => updateComposer(state, { imeComposing: false }))} onChange={(event) => setComposer((state) => updateComposer(state, { draft: event.target.value }))} /><button title="Send the first input or queue a later input" type="button" onClick={send}>Send</button><label className="attachment-field">Attachment references<input aria-label="Attachment references" title="Comma-separated local attachment references for the first input" value={composer.attachments.join(", ")} onChange={(event) => setComposer((state) => updateComposer(state, { attachments: event.target.value.split(",").map((value) => value.trim()).filter(Boolean) }))} /></label>{queuedPreview.length > 0 && <small className="queued-preview">Queued preview: {queuedPreview.join(" · ")}</small>}</div>
    <section className="chat-evidence"><h3>Evidence</h3>{evidence.items.map((record) => <button className={selectedEvidence === record.id ? "selected" : ""} key={record.id} title={`Inspect ${record.label}`} type="button" onClick={() => setSelectedEvidence(record.id)}>{record.label}</button>)}<pre>{inspectEvidence(sampleEvidence.find((record) => record.id === selectedEvidence) ?? sampleEvidence[0])}</pre></section>
    {notice !== null && <p className="chat-notice" role="status">{notice}</p>}
  </section>;
}
