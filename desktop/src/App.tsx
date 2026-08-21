import { useMemo, useState } from "react";
import type { DesktopAdapters } from "./adapters/contracts";
import { WorkflowGraphSurfaceAdapter } from "./workbench/graphSurface";
import { createEditor, selectWorkflowNode, workflowSummary } from "./workbench/workflow";
import { resolveCapabilities, type SettingsDraft } from "./workbench/settings";

interface AppProps {
  readonly adapters: DesktopAdapters;
}

const starterWorkflow = { schemaVersion: 1, nodes: [{ id: "start", label: "Start", type: "input", position: { x: 48, y: 64 } }, { id: "agent", label: "Agent", type: "model", position: { x: 320, y: 220 }, capabilityStatus: "missing" }], edges: [{ id: "start-agent", source: "start", target: "agent" }], unknownExtension: { retain: true }, comments: "Unknown fields stay lossless." } as const;
type Route = "chat" | "workflows" | "settings";

/** Compact workbench shell. Product state stays in feature kernels, not widgets. */
export function App({ adapters }: AppProps): React.JSX.Element {
  const [route, setRoute] = useState<Route>("workflows");
  const [inspectorOpen, setInspectorOpen] = useState(true);
  const [editor, setEditor] = useState(() => createEditor(starterWorkflow));
  const [draft] = useState<SettingsDraft>({ version: 1, appearance: "system", configuredCapabilities: new Set(["model.local"]) });
  const summary = workflowSummary(editor.document);
  const resolution = useMemo(() => resolveCapabilities([{ id: "model.local", label: "Local model" }, { id: "tool.files", label: "Project files" }], draft.configuredCapabilities), [draft]);
  const graph = useMemo(() => new WorkflowGraphSurfaceAdapter(), []);
  return (
    <main className="workbench">
      <nav aria-label="Primary" className="sidebar"><strong>Aworkit</strong><button onClick={() => setRoute("chat")} type="button">New Chat</button><button aria-pressed={route === "workflows"} onClick={() => setRoute("workflows")} type="button">Workflows</button><button aria-pressed={route === "settings"} onClick={() => setRoute("settings")} type="button">Settings</button><small>Connected · sequence 0</small></nav>
      <section className="content-pane">
        <header><div><p className="eyebrow">{route}</p><h1>{route === "workflows" ? "Workflow editor" : route === "settings" ? "Settings" : "New Chat"}</h1></div><button onClick={() => setInspectorOpen((open) => !open)} title="Show or hide the evidence inspector" type="button">Inspector</button></header>
        {route === "workflows" && <section className="workflow-workspace"><aside className="palette"><b>Nodes</b><button title="Drag an input node to the graph" type="button">Input</button><button title="Drag a model node to the graph" type="button">Model</button><button title="Drag a tool node to the graph" type="button">Tool</button></aside>{graph.render(editor, (id) => setEditor((state) => selectWorkflowNode(state, id)))}</section>}
        {route === "settings" && <section className="settings-panel"><h2>Capability resolution</h2><p>Draft version {draft.version}; saves only through version-checked commands.</p><ul>{resolution.available.map((item) => <li key={item.id}>Ready: {item.label}</li>)}{resolution.missing.map((item) => <li key={item.id}>Missing: {item.label}</li>)}</ul><label>Appearance<select aria-label="Appearance preference" defaultValue={draft.appearance} title="Choose System, Light, or Dark appearance"><option value="system">System</option><option value="light">Light</option><option value="dark">Dark</option></select></label></section>}
        {route === "chat" && <section className="chat-placeholder"><p>Choose a frozen workflow and provide the first input to begin a Chat.</p><textarea aria-label="Chat input" placeholder="Message Aworkit" title="Enter your Chat input" /></section>}
      </section>
      {inspectorOpen && <aside className="inspector"><h2>Evidence</h2><dl><div><dt>Workflow</dt><dd>{summary.nodes} nodes · {summary.edges} edges</dd></div><div><dt>Dependencies</dt><dd>{summary.unresolved} unresolved</dd></div><div><dt>Surface</dt><dd>{adapters.graph.name}</dd></div></dl></aside>}
    </main>
  );
}
