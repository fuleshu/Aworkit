import { useEffect, useState } from "react";
import type { ModelConfiguration } from "../configuration";

/** Keep the user's trailing newline while persisting the parsed literal list. */
function LiteralList({label,title,values,onChange}:{label:string;title:string;values:string[];onChange:(values:string[])=>void}) {
  const normalized=values.join("\n");
  const [text,setText]=useState(normalized);
  useEffect(()=>{setText(current=>current.split("\n").filter(Boolean).join("\n")===normalized?current:normalized);},[normalized]);
  return <label className="settings-field">{label}<textarea title={title} value={text} onChange={event=>{setText(event.target.value);onChange(event.target.value.split("\n").filter(Boolean));}} /></label>;
}

/** Frozen per-model policy; Context retrieval is selected through normal tools. */
export function CompressionSettings({model,onChange}:{model:ModelConfiguration;onChange:(model:ModelConfiguration)=>void}) {
  const parent=model.compaction ?? {};
  const policy=(parent.compression ?? {}) as Record<string,unknown>;
  const set=(key:string,value:unknown)=>onChange({...model,compaction:{...parent,compression:{...policy,[key]:value}}});
  const number=(key:string,label:string,fallback:number,min:number,max:number,title:string,scale=1)=><label className="settings-field">{label}<input type="number" title={title} min={min} max={max} step="1" value={Number(policy[key]??fallback)*scale} onChange={e=>{if(e.target.value!=="")set(key,Number(e.target.value)/scale);}} /></label>;
  const list=(key:string,label:string,title:string)=><LiteralList key={model.id+key} label={label} title={title} values={(policy[key]??[]) as string[]} onChange={value=>set(key,value)} />;
  return <details className="model-compression-settings"><summary>Tool result compression</summary>
    <p className="section-intro">Reduce fresh tool results before they enter context. Changes apply to new Chats. Adaptive extraction requires Context retrieval in the Agent’s selected tools.</p>
    <div className="settings-grid two-columns">
      <label className="settings-field">Compression mode<select title="Lossless retains all data; adaptive may omit retrievable content. Previously sent results remain stable." value={String(policy.mode??"lossless")} onChange={e=>set("mode",e.target.value)}><option value="off">Off</option><option value="lossless">Lossless</option><option value="adaptive">Adaptive with retrieval</option></select></label>
      <label className="settings-field">Token counter<select title="Use the context estimate or an embedded tokenizer matching your provider. These counts are separate from billed usage." value={String(policy.tokenizer??"estimate")} onChange={e=>set("tokenizer",e.target.value)}><option value="estimate">Context estimate</option><option value="o200k">o200k tokenizer</option><option value="cl100k">cl100k tokenizer</option></select></label>
      {number("minimumBytes","Minimum result bytes",2048,256,524288,"Skip compression below this size")}
      {number("minimumSavings","Minimum savings (%)",0.15,5,90,"Require this reduction in both bytes and counted tokens, including format metadata",100)}
      {number("targetRatio","Adaptive target (%)",0.4,10,90,"Target retained information fraction; protected evidence can exceed it",100)}
    </div>
    <label className="switch-label"><input type="checkbox" checked={policy.extractCode!==false} title="Use syntax trees to retain declarations and relevant function bodies; retrieve original source before edits" onChange={e=>set("extractCode",e.target.checked)} />Extract code outlines in adaptive mode</label>
    <label className="switch-label"><input type="checkbox" checked={policy.extractProse!==false} title="Retain complete relevant source spans with original positions and retrieval access" onChange={e=>set("extractProse",e.target.checked)} />Extract prose in adaptive mode</label>
    {list("protectedText","Always preserve text","One case-sensitive literal per line. Matching spans and rows remain visible in adaptive mode.")}
    {list("excludedTools","Exclude tools","One exact capability ID per line, for example tool.files.read or mcp://server/tool.")}
  </details>;
}
