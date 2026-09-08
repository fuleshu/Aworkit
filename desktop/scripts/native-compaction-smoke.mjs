// Native compaction integration: real WebView controls, frozen authority and HTTP adapters.
import assert from "node:assert/strict";
import {spawn} from "node:child_process";
import {once} from "node:events";
import {copyFile,mkdir,writeFile} from "node:fs/promises";
import {createServer} from "node:http";
import {resolve} from "node:path";
import {connectNativeWebView} from "./native-webview.mjs";
const root=resolve(`src-tauri/target/native-compaction-${Date.now()}`);
const project=resolve(root,"project");
await mkdir(project,{recursive:true});
await writeFile(resolve(project,"AGENTS.md"),"COMPACTION ROOT RULE");
await writeFile(resolve(project,"large.txt"),"Original immutable tool evidence. ".repeat(1400));
const executable=resolve(root,"aworkit-desktop.exe");
await copyFile(resolve(process.env.AWORKIT_QA_BINARY ?? "src-tauri/target/debug/aworkit-desktop.exe"),executable);
const requests=[],failures=[];let normal=0,summaries=0;
const queueMode=process.env.AWORKIT_QA_QUEUE==="1";
const modelCall=process.env.AWORKIT_QA_MODEL_CALL==="1";
const textOnly=modelCall||process.env.AWORKIT_QA_TEXT_ONLY==="1";
let releaseSummary;const summaryGate=new Promise(resolve=>{releaseSummary=resolve});
const provider=createServer(async(req,res)=>{
 try{
  if(req.method!=="POST"){res.setHeader("Content-Type","application/json");return res.end(JSON.stringify({data:[{id:"acting"},{id:"summarizer"}]}));}
  let raw="";for await(const chunk of req)raw+=chunk;
  const body=JSON.parse(raw);requests.push(body);
  let message;
  if(body.model==="summarizer"){
    summaries++;
    if(queueMode&&summaries===1)await summaryGate;
    assert.equal(body.max_tokens,8192);
    assert.ok(JSON.stringify(body).includes("compacted") || body.messages.at(-1).content.includes("summary"));
    assert.equal(Boolean(body.tools?.length),!textOnly,"auxiliary uses the same frozen tool definitions");
    message={content:summaries===1 ? "## User goal\nContinue implementation.\n"+ "Established decision and required verification. ".repeat(70) : "Continue implementation; preserve the user's requirements."};
  }else{
    normal++;
    const step=normal+(textOnly?1:0);
    if(step===1){
      const tool=body.tools.find(t=>t.function.parameters.properties.path);
      message={tool_calls:[{index:0,id:"compact.read.1",type:"function",function:{name:tool.function.name,arguments:JSON.stringify({path:"large.txt"})}}]};
    }else if(step===2){
      if(!textOnly)assert.ok(body.messages.some(m=>m.role==="tool"&&m.content.includes("Original immutable tool evidence")));
      message={content:"FIRST ANSWER RECORDED"};
    }else{
      assert.ok(JSON.stringify(body).includes("<compacted-summary>"),"follow-up uses committed checkpoint");
      assert.ok(!JSON.stringify(body).includes("EARLY CONTEXT ".repeat(30)),"shadowed prefix does not resurrect");
      if(!modelCall)assert.ok(JSON.stringify(body).includes("COMPACTION ROOT RULE"),"missing workspace instructions restored");
      if(step===3){res.statusCode=400;res.setHeader("Content-Type","application/json");return res.end(JSON.stringify({error:{code:"context_length_exceeded",message:"fixture overflow"}}));}
      message={content:step===4?"OVERFLOW RECOVERED":step===5?"REOPEN CONFIRMED":"FORK CONFIRMED"};
    }
  }
  res.setHeader("Content-Type","text/event-stream");
  res.end(`data: ${JSON.stringify({choices:[{index:0,delta:message,finish_reason:null}]})}\n\n`+
   `data: ${JSON.stringify({choices:[{index:0,delta:{},finish_reason:message.tool_calls?"tool_calls":"stop"}],usage:{prompt_tokens:1000,completion_tokens:50}})}\n\n`+"data: [DONE]\n\n");
 }catch(error){failures.push(String(error));res.statusCode=500;res.end(String(error));}
});
provider.listen(0,"127.0.0.1");await once(provider,"listening");
const origin=`http://127.0.0.1:${provider.address().port}`;
const cdpPort=9274;
let child, view, logs = "";
const delay = () => new Promise(resolve => setTimeout(resolve, 100));
async function start() {
  child = spawn(executable, [], { windowsHide: true, stdio: ["ignore", "pipe", "pipe"], env: {
    ...process.env, AWORKIT_QA_PROFILE: root, AWORKIT_QA_HIDE_WINDOW: "1",
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${cdpPort}`,
  } });
  child.stdout.on("data", chunk => { logs += chunk; }); child.stderr.on("data", chunk => { logs += chunk; });
  for (let n = 0; n < 200; n++) {
    try { view = await connectNativeWebView(`http://127.0.0.1:${cdpPort}`); return; } catch { await delay(); }
  }
  throw new Error("Native WebView did not start: " + logs);
}
async function stop() {
  view?.close(); view = undefined;
  if (child && child.exitCode === null) { const exited = once(child, "exit"); child.kill(); await exited; }
}
async function waitFor(expression) {
  for (let n = 0; n < 250; n++) {
    if (failures.length) throw new Error(failures.join("\n"));
    if (await view.evaluate(expression)) return;
    await delay();
  }
  throw new Error("Timed out: " + expression + "\n" + (await view.evaluate("document.body.innerText")).slice(-4500));
}
async function click(selector) {
  await waitFor(`Boolean(document.querySelector(${JSON.stringify(selector)} + ':not(:disabled)'))`);
  const point = await view.evaluate(`(() => { const e = document.querySelector(${JSON.stringify(selector)}); e.scrollIntoView({block:'nearest'}); const r=e.getBoundingClientRect();return {x:r.left+r.width/2,y:r.top+r.height/2}; })()`);
  await view.command("Input.dispatchMouseEvent", { type: "mousePressed", ...point, button: "left", clickCount: 1 });
  await view.command("Input.dispatchMouseEvent", { type: "mouseReleased", ...point, button: "left", clickCount: 1 });
}
async function setValue(selector, value) {
  await waitFor(`Boolean(document.querySelector(${JSON.stringify(selector)} + ':not(:disabled)'))`);
  await view.evaluate(`(() => { const e=document.querySelector(${JSON.stringify(selector)});const proto=e.tagName==='SELECT'?HTMLSelectElement.prototype:HTMLTextAreaElement.prototype;Object.getOwnPropertyDescriptor(proto,'value').set.call(e,${JSON.stringify(value)});e.dispatchEvent(new Event(e.tagName==='SELECT'?'change':'input',{bubbles:true})); })()`);
}
const snapshot = () => view.evaluate("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0})");
const settled = () => waitFor("(async()=> (await window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0})).chat.phase === 'waiting_input')()");
const panel = () => click(".context-ring-button").then(() => click(".context-display-button")).then(() => waitFor("Boolean(document.querySelector('.context-panel[open]'))"));
const closePanel = () => click('button[aria-label="Close context"]');
async function escape() {
  for (const type of ["keyDown", "keyUp"]) await view.command("Input.dispatchKeyEvent", { type, key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
}

try{
 await start();await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke)");
 await view.evaluate(`(async()=>{
  const invoke=window.__TAURI_INTERNALS__.invoke,s=await invoke('settings_snapshot');
  await invoke('settings_commit',{command:{commandId:'compact.provider',expectedVersion:s.version,appearance:'light',portableHistoryEnabled:false,provider:{baseUrl:${JSON.stringify(origin+"/v1")},model:'acting',credentialAction:'keep',apiKey:null}}});
  const v=await invoke('settings_v2_snapshot'),p=v.settings.providers[0],m=p.models[0];
  m.capabilities=['text','tools'];m.contextWindow=100000;
  p.models.push({...m,id:'model.summary',name:'Summary model',remoteId:'summarizer',compaction:null});
  m.compaction={auto:true,summarizationProvider:p.id,summarizationModel:'model.summary'};
  v.settings.approvals.defaultMode='full_access';
  for(const id of ['tool.files.read','tool.workspace_instructions'])v.settings.tools.find(t=>t.id===id).enabled=true;
  v.settings.projects=[{id:'project.compact',name:'Compaction fixture',workspace:{kind:'local_directory',location:${JSON.stringify(project)}},defaultWorkflowId:'workflow.simple-chat',portableHistoryEnabled:false}];
  await invoke('settings_v2_commit',{command:{commandId:'compact.tools',expectedVersion:v.version,settings:v.settings}});
  const w=await invoke('workflow_snapshot',{workflowId:'workflow.simple-chat'});
  w.document.nodes.find(n=>n.type==='agent').configuration.toolIds=${textOnly?"['tool.workspace_instructions']":"['tool.files.read','tool.workspace_instructions']"};
  if(${modelCall}){const n=w.document.nodes.find(n=>n.type==='agent');n.type='model_call';n.configuration={modelTierId:'tier:balanced',instructions:'Continue the task.',maximumTokens:1024};}
  await invoke('workflow_commit',{command:{commandId:'compact.workflow',expectedVersion:w.version,workflowId:'workflow.simple-chat',document:w.document}});
  await invoke('workflow_set_default',{command:{commandId:'compact.default',workflowId:'workflow.simple-chat'}});
 })()`);
 await view.command("Page.reload");
 await setValue('select[aria-label="Workflow for the first Chat input"]',"workflow.simple-chat");
 await setValue('select[aria-label="Project for the first Chat input"]',"project.compact");
 await setValue('textarea[aria-label="Chat input"]',"Read large.txt.\n"+"EARLY CONTEXT ".repeat(1800));
 await click(".composer-input .primary-action");
 await waitFor("document.querySelector('.timeline-scroll')?.textContent.includes('FIRST ANSWER RECORDED')");await settled();
 const before=await snapshot(),userCount=before.events.filter(e=>e.kind==="message.user").length;
 await click(".context-ring-button");await click(".context-compact-button");
 if(queueMode){
   await setValue('textarea[aria-label="Chat input"]',"Continue after manual checkpoint");
   await click(".composer-input .primary-action");
   await waitFor("document.querySelector('.queue-count')?.textContent.includes('1 queued')");
   assert.equal(normal,textOnly?1:2,"queued input must not race maintenance");releaseSummary();
   await waitFor("document.querySelector('.timeline-scroll')?.textContent.includes('OVERFLOW RECOVERED')");
 }
 await settled();
 await waitFor("document.querySelector('.timeline-scroll')?.textContent.includes('Context compaction')");
 assert.equal(summaries,queueMode?2:1,"manual operation and optional queued overflow use isolated summary calls");
 let after=await snapshot();
 assert.equal(after.events.filter(e=>e.kind==="message.user").length,userCount+(queueMode?1:0),"manual maintenance created no user message");
 assert.equal(after.events.filter(e=>e.kind==="message.assistant").length,queueMode?2:1,"auxiliary did not create an assistant message");
 assert.ok(after.events.some(e=>e.kind==="context.compacted"&&e.payload.strategy==="summary"));
 await panel();
 const compacted=await view.evaluate("document.querySelector('.context-source').value");
 assert.ok(compacted.includes("<compacted-summary>"));
 assert.ok(!compacted.includes("EARLY CONTEXT ".repeat(30)));
 await view.screenshot(resolve(root,"manual-context.png"));await closePanel();
 if(!queueMode){await setValue('textarea[aria-label="Chat input"]',"Continue after manual checkpoint");
 await click(".composer-input .primary-action");
 await waitFor("document.querySelector('.timeline-scroll')?.textContent.includes('OVERFLOW RECOVERED')");await settled();}
 assert.equal(normal,textOnly?3:4);assert.equal(summaries,2,"canonical overflow caused one isolated reduction and one retry");
 await stop();await start();
 await setValue('textarea[aria-label="Chat input"]',"Verify after restart");
 await click(".composer-input .primary-action");
 await waitFor("document.querySelector('.timeline-scroll')?.textContent.includes('REOPEN CONFIRMED')");await settled();
 after=await snapshot();assert.deepEqual(failures,[]);
 const started=after.events.filter(e=>e.kind==="context.compaction-started"),ended=after.events.filter(e=>e.kind==="context.compaction-ended");
 assert.equal(started.length,ended.length);
 assert.ok(ended.every(e=>!e.payload.error),JSON.stringify(ended));
 const parent=after.chat.chatId;
 await view.evaluate(`(async()=>{const invoke=window.__TAURI_INTERNALS__.invoke,s=await invoke('desktop_snapshot',{afterSequence:0});await invoke('desktop_command',{command:{schemaVersion:1,commandId:'compact.fork',expectedVersion:s.version,action:'fork',targetId:s.chat.chatId,payload:{}}})})()`);
 await view.command("Page.reload");
 assert.notEqual((await snapshot()).chat.chatId,parent);
 await setValue('textarea[aria-label="Chat input"]',"Continue the fork");await click(".composer-input .primary-action");
 await waitFor("document.querySelector('.timeline-scroll')?.textContent.includes('FORK CONFIRMED')");await settled();
 assert.equal(summaries,2,"fork inherits compacted selection");
 await writeFile(resolve(root,"requests.json"),JSON.stringify(requests,null,2));
 const report={ok:true,root,queueMode,textOnly,modelCall,providerRequests:requests.length,normal,summaries,cases:["native manual control","separate frozen summary model","no auxiliary Chat messages","original history retained","Display Context checkpoint","typed HTTP overflow reduction and retry",...(!modelCall?["workspace rules restored"]:[]),"restart continuation","fork selection and provenance","balanced lifecycle",...(queueMode?["queued input waits for maintenance"]:[])]};
 await writeFile(resolve(root,"report.json"),JSON.stringify(report,null,2));console.log(JSON.stringify(report,null,2));
}catch(error){if(view){await writeFile(resolve(root,"failure-snapshot.json"),JSON.stringify(await snapshot(),null,2));await view.screenshot(resolve(root,"failure.png"));console.log("failure profile",root);}throw error;}finally{await stop();await writeFile(resolve(root,"native.log"),logs);await writeFile(resolve(root,"requests.json"),JSON.stringify(requests,null,2));provider.closeAllConnections();provider.close();}
