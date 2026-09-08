// Actual WebView workflow selection/validation plus durable provider payload proof.
import assert from "node:assert/strict";
import {spawn} from "node:child_process";
import {once} from "node:events";
import {copyFile,mkdir,writeFile} from "node:fs/promises";
import {createServer} from "node:http";
import {resolve} from "node:path";
import {connectNativeWebView} from "./native-webview.mjs";
const root=resolve(`src-tauri/target/native-compression-${Date.now()}`),project=resolve(root,"project");
await mkdir(project,{recursive:true});
const source=Array.from({length:45},(_,i)=>`fn worker_${i}() {\n${Array.from({length:24},(_,j)=>`    let measurement_${i}_${j} = ${i*100+j};`).join("\n")}\n${i===17?'    // hidden_receipt_992 = exact-original-proof\n':''}}\n`).join("\n")+'fn target_739() { println!("TARGET MUST SURVIVE"); }\n';
await writeFile(resolve(project,"source.rs"),source);await writeFile(resolve(project,"AGENTS.md"),"COMPRESSION WORKSPACE RULE");
const executable=resolve(root,"aworkit-desktop.exe");
await copyFile(resolve(process.env.AWORKIT_QA_BINARY??"src-tauri/target/debug/aworkit-desktop.exe"),executable);
const requests=[],failures=[];let step=0,reference,firstProjection,summaries=0;
function call(name,id,args){return {tool_calls:[{index:0,id,type:"function",function:{name,arguments:JSON.stringify(args)}}]};}
const provider=createServer(async(req,res)=>{
 try{
  if(req.method!=="POST"){res.setHeader("Content-Type","application/json");return res.end(JSON.stringify({data:[{id:"acting"},{id:"summary"}]}));}
  let raw="";for await(const chunk of req)raw+=chunk;
  const body=JSON.parse(raw);requests.push(body);let message;
  if(body.model==="summary"){summaries++;message={content:"Continue verification. Earlier source inspection succeeded. Original evidence is available through Context retrieval search."};}
  else{
   step++;
   const tools=body.tools??[];assert.ok(tools.some(t=>t.function.name==="aworkit_context"));
   const results=body.messages.filter(m=>m.role==="tool");
   if(step===1)message=call("aworkit_read_project_file","compress.read",{path:"source.rs"});
   else if(step===2){
    firstProjection=results.at(-1).content;const output=JSON.parse(firstProjection);reference=output.aworkitContext.reference;
    assert.equal(reference.length,64);assert.equal(output.aworkitContext.omitted,true);
    assert.ok(firstProjection.includes("TARGET MUST SURVIVE"));assert.ok(!firstProjection.includes("hidden_receipt_992"));
    assert.ok(firstProjection.length<source.length/2);
    message=call("aworkit_context","compress.search",{operation:"search",reference,pointer:"/content",query:"hidden_receipt_992"});
   }else if(step===3){
    assert.equal(results[0].content,firstProjection,"previous tool content must be cache-stable");
    assert.ok(results.at(-1).content.includes("exact-original-proof"));
    message=call("aworkit_context","compress.stats",{operation:"stats"});
   }else if(step===4){
    assert.equal(JSON.parse(results.at(-1).content).compressedResults,1);
    assert.equal(results[0].content,firstProjection);message={content:"COMPRESSION VERIFIED"};
   }else if(step===5 || step===7){
    assert.ok(JSON.stringify(body).includes("<compacted-summary>"));
    message=call("aworkit_context",`compress.recover.${step}`,{operation:"search",query:"hidden_receipt_992",pointer:"/content"});
   }else if(step===6 || step===8){
    const output=JSON.parse(results.at(-1).content);assert.equal(output.matches[0].reference,reference);
    assert.ok(JSON.stringify(output).includes("exact-original-proof"));
    message={content:step===6?"RESTART RETRIEVAL VERIFIED":"FORK RETRIEVAL VERIFIED"};
   }else throw new Error("unexpected acting request "+step);
  }
  res.setHeader("Content-Type","text/event-stream");
  res.end(`data: ${JSON.stringify({choices:[{index:0,delta:message,finish_reason:null}]})}\n\n`+`data: ${JSON.stringify({choices:[{index:0,delta:{},finish_reason:message.tool_calls?"tool_calls":"stop"}],usage:{prompt_tokens:1000,completion_tokens:50}})}\n\n`+"data: [DONE]\n\n");
 }catch(error){failures.push(String(error));res.statusCode=500;res.end(String(error));}
});
provider.listen(0,"127.0.0.1");await once(provider,"listening");const origin=`http://127.0.0.1:${provider.address().port}`;
let child,view,logs="";const delay=()=>new Promise(r=>setTimeout(r,100));
async function start(){
 child=spawn(executable,[],{windowsHide:true,stdio:["ignore","pipe","pipe"],env:{...process.env,AWORKIT_QA_PROFILE:root,AWORKIT_QA_HIDE_WINDOW:"1",WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:"--remote-debugging-port=9276"}});
 child.stdout.on("data",c=>logs+=c);child.stderr.on("data",c=>logs+=c);
 for(let n=0;n<200;n++){try{view=await connectNativeWebView("http://127.0.0.1:9276");return;}catch{await delay();}}
 throw new Error("Native WebView failed: "+logs);
}
async function stop(){view?.close();view=undefined;if(child&&child.exitCode===null){const end=once(child,"exit");child.kill();await end;}}
async function waitFor(expression){for(let n=0;n<300;n++){if(failures.length)throw new Error(failures.join("\n"));if(await view.evaluate(expression))return;await delay();}throw new Error("Timeout: "+expression+"\n"+(await view.evaluate("document.body.innerText")).slice(-5000));}
async function click(name){
 const find=`(()=>{const name=${JSON.stringify(name)},buttons=[...document.querySelectorAll('button')].filter(b=>!b.disabled&&b.getClientRects().length);return buttons.find(b=>b.title===name||b.textContent.trim()===name||b.getAttribute('aria-label')===name)??buttons.find(b=>b.textContent.trim().startsWith(name));})()`;
 await waitFor(`Boolean(${find})`);
 const point=await view.evaluate(`(()=>{const b=${find};b.scrollIntoView({block:'nearest',behavior:'instant'});const r=b.getBoundingClientRect();return {x:r.left+r.width/2,y:r.top+r.height/2};})()`);
 await pointerClick(point);
}
async function pointerClick(point){for(const type of ['mousePressed','mouseReleased'])await view.command('Input.dispatchMouseEvent',{type,...point,button:'left',clickCount:1});}
async function selectorClick(selector){await waitFor(`Boolean(document.querySelector(${JSON.stringify(selector)}+':not(:disabled)')?.getClientRects().length)`);const point=await view.evaluate(`(()=>{const b=document.querySelector(${JSON.stringify(selector)});b.scrollIntoView({block:'center',behavior:'instant'});const r=b.getBoundingClientRect();return {x:r.left+r.width/2,y:r.top+r.height/2};})()`);await pointerClick(point);}
async function setValue(selector,value){await waitFor(`Boolean(document.querySelector(${JSON.stringify(selector)}))`);await view.evaluate(`(()=>{const e=document.querySelector(${JSON.stringify(selector)});Object.getOwnPropertyDescriptor(e.tagName==='SELECT'?HTMLSelectElement.prototype:HTMLTextAreaElement.prototype,'value').set.call(e,${JSON.stringify(value)});e.dispatchEvent(new Event(e.tagName==='SELECT'?'change':'input',{bubbles:true}));})()`);}
const snapshot=()=>view.evaluate("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0})");
const settled=()=>waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.phase==='waiting_input')");
async function send(text,answer){await setValue('textarea[aria-label="Chat input"]',text);await selectorClick('.composer-input .primary-action');await waitFor(`document.querySelector('.timeline-scroll')?.textContent.includes(${JSON.stringify(answer)})`);await settled();}
try{
 await start();await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke)");
 const setup=await view.evaluate(`(async()=>{try{const invoke=window.__TAURI_INTERNALS__.invoke,s=await invoke('settings_snapshot');
  await invoke('settings_commit',{command:{commandId:'compression.provider',expectedVersion:s.version,appearance:'light',portableHistoryEnabled:false,provider:{baseUrl:${JSON.stringify(origin+"/v1")},model:'acting',credentialAction:'keep',apiKey:null}}});
  const v=await invoke('settings_v2_snapshot'),p=v.settings.providers[0],m=p.models[0];m.capabilities=['text','tools'];m.contextWindow=100000;
  p.models.push({...m,id:'model.summary',name:'Summary model',remoteId:'summary'});
  m.compaction={auto:true,pruneToolResults:false,summarizationProvider:p.id,summarizationModel:'model.summary',compression:{mode:'lossless',tokenizer:'o200k',targetRatio:0.4}};
  v.settings.approvals.defaultMode='full_access';
  for(const id of ['tool.context','tool.files.read','tool.workspace_instructions'])v.settings.tools.find(t=>t.id===id).enabled=true;
  v.settings.projects=[{id:'project.compression',name:'Compression fixture',workspace:{kind:'local_directory',location:${JSON.stringify(project)}},defaultWorkflowId:'workflow.simple-chat',portableHistoryEnabled:false}];
  await invoke('settings_v2_commit',{command:{commandId:'compression.settings',expectedVersion:v.version,settings:v.settings}});
  const w=await invoke('workflow_snapshot',{workflowId:'workflow.simple-chat'});w.document.nodes.find(n=>n.type==='agent').configuration.toolIds=['tool.files.read','tool.workspace_instructions'];
  await invoke('workflow_commit',{command:{commandId:'compression.workflow',expectedVersion:w.version,workflowId:'workflow.simple-chat',document:w.document}});
  await invoke('workflow_set_default',{command:{commandId:'compression.default',workflowId:'workflow.simple-chat'}});
 return null;}catch(error){return String(error);}})()`);
 assert.equal(setup,null,"fixture setup: "+setup);
 await view.command("Page.reload");await click("Settings");
 await selectorClick('.model-compression-settings summary');
 await waitFor("Boolean(document.querySelector('.model-compression-settings[open]'))");
 await setValue('.model-compression-settings select','adaptive');await click('Save configuration');
 await waitFor("window.__TAURI_INTERNALS__.invoke('settings_v2_snapshot').then(v=>v.settings.providers[0].models[0].compaction.compression.mode==='adaptive')");
 await waitFor("document.querySelector('.settings-toolbar-actions span')?.textContent==='Version 4 · saved'&&[...document.querySelectorAll('.settings-toolbar-actions button')].some(b=>b.textContent==='Save configuration'&&b.disabled)");
 assert.equal(await view.evaluate("document.querySelector('.model-compression-settings select').value"),"adaptive");
 if(!await view.evaluate("document.querySelector('.model-compression-settings').open"))await selectorClick('.model-compression-settings summary');
 await waitFor("Boolean(document.querySelector('.model-compression-settings[open]'))");
 await view.evaluate("document.querySelector('.model-compression-settings').scrollIntoView({block:'start',behavior:'instant'})");
 await view.evaluate("new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(()=>resolve(true))))");
 await view.screenshot(resolve(root,"model-settings.png"));await click("Tools");
 await waitFor("Boolean(document.querySelector('[id=\"tool.context-maximumBytes\"]'))");await view.screenshot(resolve(root,"tool-settings.png"));await click("Back to Chat");
 await click("Workflows");await setValue(".workflow-library-bar select","workflow.simple-chat");
 await waitFor("document.body.innerText.includes('Version 2')");
 await view.evaluate("[...document.querySelectorAll('[aria-label=\"Workflow nodes\"] button')].find(b=>b.textContent==='Agent').click()");
 await selectorClick('input[title="Bind Context retrieval to this agent"]');await click("Validate");
 await waitFor("document.body.innerText.includes('Validation passed: this workflow document is executable.')");await view.screenshot(resolve(root,"workflow-validation.png"));
 await click("Save");await waitFor("window.__TAURI_INTERNALS__.invoke('workflow_snapshot',{workflowId:'workflow.simple-chat'}).then(w=>w.document.nodes.find(n=>n.type==='agent').configuration.toolIds.includes('tool.context'))");
 const priorChat=(await snapshot()).chat.chatId;await click("Run");
 await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId!==${JSON.stringify(priorChat)}&&document.body.innerText.includes(s.chat.runId)&&!document.querySelector('.workflow-library-bar')?.getClientRects().length)`);
 await setValue('select[aria-label="Workflow for the first Chat input"]',"workflow.simple-chat");await setValue('select[aria-label="Project for the first Chat input"]',"project.compression");
 await send("Read source.rs and describe target_739.","COMPRESSION VERIFIED");
 let state=await snapshot();const archive=state.events.find(e=>e.kind==="context.compression");assert.ok(archive);assert.equal(archive.payload.original.content,source);
 assert.equal(state.events.filter(e=>e.kind==="context.compression").length,1,"retrieval does not recursively compress");
 await selectorClick('.context-ring-button');await waitFor("Boolean(document.querySelector('.context-compression-usage'))");await view.screenshot(resolve(root,"compression-usage.png"));
 await selectorClick('.context-compact-button');await settled();assert.equal(summaries,1);
 await stop();await start();await send("Recover original evidence after restart.","RESTART RETRIEVAL VERIFIED");
 const parent=(await snapshot()).chat.chatId;
 await view.evaluate(`(async()=>{const invoke=window.__TAURI_INTERNALS__.invoke,s=await invoke('desktop_snapshot',{afterSequence:0});await invoke('desktop_command',{command:{schemaVersion:1,commandId:'compression.fork',expectedVersion:s.version,action:'fork',targetId:s.chat.chatId,payload:{}}});})()`);
 await view.command("Page.reload");assert.notEqual((await snapshot()).chat.chatId,parent);await send("Recover original evidence in this fork.","FORK RETRIEVAL VERIFIED");
 state=await snapshot();assert.ok(state.events.some(e=>e.kind==="context.compression"&&e.payload.parentChatId===parent));assert.deepEqual(failures,[]);
 const report={ok:true,root,providerRequests:requests.length,metrics:archive.payload.metrics,cases:["native plugin Settings","workflow bind Validate Save Run","AST extraction before provider cap","immutable original evidence","byte-stable previous tool message","exact original search","retrieval bypass","local savings UI","summary followed by reference discovery","restart retrieval","fork archive authority"]};
 await writeFile(resolve(root,"report.json"),JSON.stringify(report,null,2));console.log(JSON.stringify(report,null,2));
}catch(error){if(view){await writeFile(resolve(root,"failure-snapshot.json"),JSON.stringify(await snapshot(),null,2));await view.screenshot(resolve(root,"failure.png"));}console.error("Evidence:",root);throw error;}
finally{await stop();await writeFile(resolve(root,"requests.json"),JSON.stringify(requests,null,2));await writeFile(resolve(root,"native.log"),logs);provider.closeAllConnections();provider.close();}
