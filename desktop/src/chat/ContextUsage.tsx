import { useEffect, useId, useMemo, useRef, useState } from "react";
import type { RuntimeEvent } from "./corePort";
import { ContextPanel } from "./ContextPanel";
import { CompressionUsage } from "./CompressionUsage";
import { compactTokens, contextModel, contextUsage, projectContexts, type ContextDocument, type ContextModel, type ContextSelection } from "./contextProjection";
import "./context.css";

interface Props {
  readonly events: readonly RuntimeEvent[];
  readonly model?: ContextModel | null;
  readonly editDisabledReason: string | null;
  readonly onSave: (selection: ContextSelection, document: ContextDocument) => Promise<boolean>;
  readonly onCompact?: (selection: ContextSelection) => Promise<boolean>;
}

/** Compact context ring and anchored disclosure, driven by canonical model-call events. */
export function ContextUsage({ events, model: fallback, editDisabledReason, onSave, onCompact }: Props): React.JSX.Element {
  const selections = useMemo(() => projectContexts(events), [events]);
  const [selectedNode, setSelectedNode] = useState<string | null>(null);
  const selection = selections.find(s => s.nodeId === selectedNode) ?? selections[0];
  const model = contextModel(events, fallback);
  const estimate = selection ? contextUsage(selection) : null;
  const capacity = model?.contextWindow ?? null;
  const percent = capacity && estimate ? Math.round(estimate.total / capacity * 100) : null;
  const [open, setOpen] = useState(false);
  const [panel, setPanel] = useState<ContextSelection | null>(null);
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const popupId = useId();
  useEffect(() => {
    if (!open) return;
    const dismiss = (event: PointerEvent) => { if (event.target instanceof Node && !root.current?.contains(event.target)) setOpen(false); };
    document.addEventListener("pointerdown", dismiss);
    return () => document.removeEventListener("pointerdown", dismiss);
  }, [open]);
  const label = percent === null ? "Context usage" : `Context usage: approximately ${percent}% used`;
  return <div ref={root} className="context-usage" onKeyDown={event => {
    if (event.key === "Escape" && open) { event.stopPropagation(); setOpen(false); trigger.current?.focus(); }
  }}>
    <button ref={trigger} className="context-ring-button" type="button" aria-label={label}
      title={label + " — click for details"} aria-expanded={open} aria-controls={popupId} aria-haspopup="dialog" onClick={() => setOpen(!open)}>
      <svg width="23" height="23" viewBox="0 0 24 24" aria-hidden="true">
        <circle className="context-ring-track" cx="12" cy="12" r="8" />
        <circle className={"context-ring-fill" + (percent !== null && percent >= 90 ? " context-ring-high" : "")}
          cx="12" cy="12" r="8" pathLength="100" strokeDasharray={`${percent === null ? 0 : Math.min(100, percent)} 100`} />
      </svg>
    </button>
    {open && <section id={popupId} role="dialog" aria-label="Context usage details" className="context-popover">
      <div className="context-popover-summary"><span><strong>{percent === null ? "—" : `${percent}%`}</strong> of context used</span>
        <strong>{estimate ? (estimate.reported ? "" : "~") + compactTokens(estimate.total) : "—"} / {capacity ? compactTokens(capacity) : "Unknown"}</strong></div>
      <div className="context-segments" aria-hidden="true">{estimate && ["system", "tools", "messages"].map(part => {
        const amount = estimate[part as "system" | "tools" | "messages"];
        return <span key={part} className={"context-segment-" + part} style={{ width: `${Math.min(100, amount / Math.max(capacity ?? estimate.total, estimate.total, 1) * 100)}%` }} />;
      })}</div>
      <dl className="context-breakdown">{(["system", "tools", "messages"] as const).map(part =>
        <div key={part}><dt><i className={"context-segment-" + part} />{part === "system" ? "System prompt" : part === "tools" ? "Tools" : "Messages"}</dt><dd>{estimate ? "~" + compactTokens(estimate[part]) : "—"}</dd></div>)}</dl>
      {selections.length > 1 && <select aria-label="Context workflow node" title="Choose the workflow node whose model context you want to inspect" value={selection?.nodeId}
        onChange={event => setSelectedNode(event.target.value)}>{selections.map(s => <option key={s.nodeId} value={s.nodeId}>{s.label}</option>)}</select>}
      <p className="context-usage-note">{estimate ? estimate.reported ? "Provider total · estimated breakdown." : "Estimated tokens · includes the latest completed response." : "Context is available after the first model call."}
        {estimate?.hasImages && !estimate.reported && " Image token cost is not included in the estimate."}
        {!capacity && " This model has no configured context limit."}</p>
      {selection?.inputTokens !== null && selection?.inputTokens !== undefined && <p className="context-usage-note">Last request reported {compactTokens(selection.inputTokens)} input / {compactTokens(selection.outputTokens ?? 0)} output tokens.</p>}
      {model && <p className="context-model-name">{model.name}</p>}
      <CompressionUsage events={events} nodeId={selection?.nodeId} />
      <button type="button" className="context-display-button" disabled={!selection} title="Open the complete raw model context"
        onClick={() => { if (selection) { setPanel(selection); setOpen(false); } }}>Display Context</button>
      {onCompact && <button type="button" className="context-compact-button" disabled={!selection || Boolean(editDisabledReason)} title={editDisabledReason ?? "Summarize earlier context now, retaining recent work and the original Chat history"}
        onClick={() => { if (selection) { setOpen(false); void onCompact(selection); } }}>Compact context</button>}
    </section>}
    {panel && <ContextPanel selection={panel} editDisabledReason={editDisabledReason} onSave={onSave} onClose={() => { setPanel(null); trigger.current?.focus(); }} />}
  </div>;
}
