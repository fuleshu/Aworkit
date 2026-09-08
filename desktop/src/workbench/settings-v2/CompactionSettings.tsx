import type { ModelConfiguration, ProviderConfiguration } from "../configuration";
import { CompressionSettings } from "./CompressionSettings";

export function CompactionSettings({model,providers,onChange}:{model:ModelConfiguration;providers:readonly ProviderConfiguration[];onChange:(model:ModelConfiguration)=>void}) {
  const policy = model.compaction ?? {};
  const set = (key:string,value:unknown) => onChange({...model,compaction:{...policy,[key]:value}});
  const number = (key:string,label:string,fallback:number,title:string,scale=1,min=0,max=1_048_576) => <label className="settings-field" key={key}>
    {label}<input type="number" title={title} min={min} max={max} step="1" value={Number(policy[key] ?? fallback)*scale}
      onChange={event=> { if(event.target.value!=="") set(key,Number(event.target.value)/scale); }} />
  </label>;
  return <><CompressionSettings model={model} onChange={onChange} /><details className="model-compaction-settings"><summary>Context compaction</summary>
    <p className="section-intro">Summarize earlier work as this model approaches its context limit. The original Chat history remains available. Changes apply to new Chats.</p>
    <label className="switch-label"><input type="checkbox" checked={policy.auto !== false} onChange={event=>set("auto",event.target.checked)} title="Automatically reduce context before model requests" />Automatic compaction</label>
    <div className="settings-grid two-columns">
      <label className="settings-field">Summary model<select title="Use the acting model or freeze a separate provider and model for summaries" value={policy.summarizationProvider ? JSON.stringify([policy.summarizationProvider,policy.summarizationModel]) : ""}
        onChange={event=>{const next={...policy};delete next.summarizationProvider;delete next.summarizationModel;if(event.target.value){const [providerId,modelId]=JSON.parse(event.target.value) as string[];next.summarizationProvider=providerId;next.summarizationModel=modelId;}onChange({...model,compaction:next});}}>
        <option value="">Acting model</option>{providers.filter(p=>p.enabled).flatMap(p=>p.models.filter(m=>m.enabled).map(m=><option key={`${p.id}/${m.id}`} value={JSON.stringify([p.id,m.id])}>{p.name} / {m.name}</option>))}
      </select></label>
      {number("thresholdRatio","Compact at (%)",0.8,"Percentage of the model context window that triggers compaction",100,1,100)}
      <label className="settings-field">Retained history budget<select value={policy.retainTokens == null ? "ratio":"tokens"} title="Keep a recent verbatim tail as a fraction of capacity or an absolute token budget"
        onChange={event=> { const next={...policy}; delete next.retainTokens; delete next.retainRatio; if(event.target.value==="tokens")next.retainTokens=4096;onChange({...model,compaction:next}); }}><option value="ratio">Percentage</option><option value="tokens">Tokens</option></select></label>
      {policy.retainTokens == null ? number("retainRatio","Retain recent history (%)",0.16,"Minimum recent history to preserve verbatim; must be below the trigger",100,1,99) : number("retainTokens","Retain recent tokens",4096,"Minimum recent history to preserve verbatim")}
      {number("maxTokens","Summary token limit",8192,"Maximum output tokens for the checkpoint summary",1,1)}
      {number("compactionRetries","Additional pressure reductions",1,"Additional reductions if a valid checkpoint still leaves context above the threshold",1,0,32)}
      {number("maxOverflowRetries","Overflow recovery attempts",1,"Consecutive provider context-overflow retries; each requires a committed reduction",1,0,32)}
    </div>
    <label className="switch-label"><input type="checkbox" checked={policy.pruneToolResults !== false} onChange={event=>set("pruneToolResults",event.target.checked)} title="Before summarizing, reduce large tool outputs by preserving their beginning and end" />Prune large tool results first</label>
    <div className="settings-grid two-columns">
      {number("thresholdChars","Tool result character threshold",8192,"Prune text exceeding this many Unicode characters",1,1,4*1024*1024)}
      {number("headChars","Keep beginning characters",4096,"Characters preserved from the beginning of a large tool result")}
      {number("tailChars","Keep ending characters",1024,"Characters preserved from the end of a large tool result")}
    </div>
  </details></>;
}
