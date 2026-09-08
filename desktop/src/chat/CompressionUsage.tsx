import type { RuntimeEvent } from "./corePort";
import { compactTokens } from "./contextProjection";

/** Savings are one-time representation deltas, never a provider billing claim. */
export function CompressionUsage({events,nodeId}:{events:readonly RuntimeEvent[];nodeId:string|undefined}) {
  const facts=events.map(e=>({kind:e.kind,payload:(e.payload && typeof e.payload==="object" ? e.payload : {}) as Record<string,unknown>}));
  const records=facts.filter(e=>e.kind==="context.compression" && e.payload.nodeId===nodeId && e.payload.child==null);
  if(!records.length)return null;
  let before=0,after=0;
  for(const event of records){const metrics=event.payload.metrics as {beforeBytes?:number;afterBytes?:number}|undefined;before+=metrics?.beforeBytes??0;after+=metrics?.afterBytes??0;}
  const reads=facts.filter(e=>e.kind==="context.retrieved"&&e.payload.nodeId===nodeId&&e.payload.child==null).length;
  return <p className="context-compression-usage context-usage-note" title="Cumulative local representation savings for this node; excludes retrieved pages, summaries, provider caching and billed tokens.">
    Tool compression: {compactTokens(Math.max(0,before-after))} bytes saved across {records.length} {records.length===1?"result":"results"} · {reads} {reads===1?"retrieval":"retrievals"}.
  </p>;
}
