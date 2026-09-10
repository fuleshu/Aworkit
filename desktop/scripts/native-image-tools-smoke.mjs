// End-to-end proof in an isolated native profile with a local vision provider.
// Captures only this fixture's own visible Aworkit window.
import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { mkdir, readFile, writeFile, unlink } from "node:fs/promises";
import { createHash } from "node:crypto";
import { resolve } from "node:path";
import { once } from "node:events";
import assert from "node:assert/strict";
import { connectNativeWebView } from "./native-webview.mjs";

const root = resolve(`src-tauri/target/native-image-tools-${Date.now()}`);
const project = resolve(root, "project");
await mkdir(resolve(project, ".git"), { recursive: true });
await writeFile(resolve(project, ".git/HEAD"), "ref: refs/heads/main\n");
const png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";
await writeFile(resolve(project, "red.png"), Buffer.from(png, "base64"));
// Optional regression: no project, a recoverable bad call, then an oversized
// absolute image outside the Chat workspace. Only this isolated copy is removed.
const localPaths = process.argv.includes("--local-paths");
const externalPath = resolve(root, "external image ü.png");
const hash = bytes => createHash("sha256").update(bytes).digest("hex");
let sourceBytes, sourceHash, imageName, imageData, prepared;
if (localPaths) {
  sourceBytes = process.env.AWORKIT_QA_SOURCE_IMAGE
    ? await readFile(process.env.AWORKIT_QA_SOURCE_IMAGE)
    : Buffer.concat([Buffer.from(png, "base64"), Buffer.alloc(6 * 1024 * 1024)]);
  sourceHash = hash(sourceBytes);
  await writeFile(externalPath, sourceBytes);
}
const toolTurns = localPaths ? 2 : 3;
const imageCount = localPaths ? 1 : 2;
const requests = [], failures = [];
const startupLog = [];
let child, view, screenshotData;
const provider = createServer(async (request, response) => {
  try {
    if (request.url === "/v1/models") return response.end(JSON.stringify({data:[{id:"vision-fixture",supports_vision:true}]}));
    const chunks=[];for await(const chunk of request)chunks.push(chunk);
    const body=JSON.parse(Buffer.concat(chunks).toString());requests.push(body);
    const results=body.messages.filter(m=>m.role==="tool");
    const images=body.messages.flatMap(m=>Array.isArray(m.content)?m.content.filter(p=>p.type==="image_url"):[]);
    assert.ok(body.tools.some(t=>t.function.name==="aworkit_read_image"));
    assert.ok(body.tools.some(t=>t.function.name==="aworkit_screenshot"));
    let name="aworkit_read_image",args={path:resolve(project,"red.png")};
    if (localPaths) {
      args = {path: 42};
      if (results.length >= 1) {
        assert.match(results[0].content, /path.*text/i, "invalid input returned to the model");
        args = {path: externalPath};
      }
      if (results.length >= 2) {
        assert.equal(images.length, 1);
        const data = images[0].image_url.url;
        const content = JSON.parse(results[1].content);
        const bytes = Buffer.from(data.split(",")[1], "base64");
        assert.equal(bytes.length, content.image.byteLength);
        assert.equal(hash(bytes), content.image.id);
        assert.ok(bytes.length <= 5 * 1024 * 1024);
        assert.equal(content.source.preparation.originalBytes, sourceBytes.length);
        assert.equal(content.source.preparation.originalSha256, sourceHash);
        assert.equal(content.source.preparation.reencoded, true);
        if (imageData) assert.equal(data, imageData, "restart reuses the saved image copy");
        else {
          assert.equal(hash(await readFile(externalPath)), sourceHash, "read preserves the source");
          await unlink(externalPath);
          await writeFile(resolve(root, content.image.name), bytes);
        }
        imageData = data; imageName = content.image.name; prepared = content.source.preparation;
      } else assert.equal(images.length, 0);
    }
    if(!localPaths && results.length>=1){
      assert.equal(images[0].image_url.url,`data:image/png;base64,${png}`);
      assert.ok(body.messages.indexOf(results[0])<body.messages.findIndex(m=>Array.isArray(m.content)&&m.content.some(p=>p.type==="image_url")));
    }
    if(!localPaths && results.length===1){await unlink(resolve(project,"red.png"));name="aworkit_screenshot";args={operation:"list"};}
    if(!localPaths && results.length===2){
      let listed=JSON.parse(results[1].content);
      // The normal lossless output compressor can encode repeated target rows.
      if(listed.format==='aworkit.table.v1'){
        const table=listed.tables.find(t=>t.pointer==='/targets');
        listed={...listed.data,targets:listed.data.targets.map(row=>({...table.constants,...Object.fromEntries(table.columns.map((key,i)=>[key,row[i]]))}))};
      }
      assert.ok(listed.targets.some(t=>t.kind==="monitor"));
      const target=listed.targets.find(t=>t.target.startsWith(`window:${child.pid}:`));
      assert.ok(target,"own fixture window is selectable");
      name="aworkit_screenshot";args={operation:"capture",target:target.target};
    }
    if(!localPaths && results.length>=3){
      assert.equal(images.length,2);
      const data=images[1].image_url.url;
      const captured=JSON.parse(results[2].content);
      assert.ok(captured.source.target.startsWith("window:"));
      const bytes=Buffer.from(data.split(",")[1],"base64");
      assert.equal(bytes.readUInt32BE(16),captured.source.width);
      assert.equal(bytes.readUInt32BE(20),captured.source.height);
      assert.ok(captured.source.width>300&&captured.source.height>300);
      if(screenshotData)assert.equal(data,screenshotData,"restart must reuse settled screenshot");
      screenshotData=data;
      await writeFile(resolve(root,"captured-window.png"),bytes);
    }
    const delta=results.length>=toolTurns?{role:"assistant",content:"Image tools verified: saved image bytes reached the model."}
      :{role:"assistant",tool_calls:[{index:0,id:`image.${results.length}`,type:"function",function:{name,arguments:JSON.stringify(args)}}]};
    response.setHeader("Content-Type","text/event-stream");
    response.write(`data: ${JSON.stringify({choices:[{index:0,delta,finish_reason:null}]})}\n\n`);
    response.write(`data: ${JSON.stringify({choices:[{index:0,delta:{},finish_reason:results.length>=toolTurns?"stop":"tool_calls"}],usage:{prompt_tokens:100,completion_tokens:20,total_tokens:120}})}\n\n`);
    response.end("data: [DONE]\n\n");
  }catch(error){failures.push(String(error));response.statusCode=500;response.end(String(error));}
});
provider.listen(0,"127.0.0.1");await once(provider,"listening");
const origin=`http://127.0.0.1:${provider.address().port}`;
// A unique debug port prevents connecting to a WebView left by an earlier run.
const reservation=createServer();reservation.listen(0,'127.0.0.1');await once(reservation,'listening');
const port=reservation.address().port;await new Promise(resolve=>reservation.close(resolve));
const executable=process.env.AWORKIT_QA_EXECUTABLE??resolve("src-tauri/target/debug/aworkit-desktop.exe");
const pause=()=>new Promise(r=>setTimeout(r,100));
async function boot(){
  const env={...process.env,AWORKIT_QA_PROFILE:root,AWORKIT_QA_TOPMOST:'1',WEBVIEW2_USER_DATA_FOLDER:resolve(root,'webview'),WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:`--remote-debugging-port=${port}`};
  delete env.AWORKIT_QA_HIDE_WINDOW;
  child=spawn(executable,[],{windowsHide:true,stdio:["ignore","pipe","pipe"],env});
  child.stdout.on('data', chunk=>startupLog.push(chunk.toString()));
  child.stderr.on('data', chunk=>startupLog.push(chunk.toString()));
  child.on('error', error=>startupLog.push(String(error)));
  let connectionError;
  for(let i=0;i<300&&!view;i++){
    if(child.exitCode!==null)break;
    try{view=await connectNativeWebView(`http://127.0.0.1:${port}`);}catch(error){connectionError=String(error);await pause();}
  }
  assert.ok(view,`native WebView started (exit=${child.exitCode}, ${connectionError}): ${startupLog.join('')}`);
  await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke)");
}
async function stop(){
  view?.close();view=undefined;
  if(child?.exitCode===null){
    const exited=once(child,'exit');
    const killer=spawn(resolve(process.env.SystemRoot,'System32/taskkill.exe'),['/PID',String(child.pid),'/T','/F'],{windowsHide:true,stdio:'ignore'});
    await once(killer,'exit');await exited;
  }
}
async function waitFor(expression){
  for(let i=0;i<300;i++){if(await view.evaluate(expression))return;if(failures.length)throw Error(failures.join("\n"));await pause();}
  const diagnostics=await view.evaluate("({text:document.body.innerText,buttons:[...document.querySelectorAll('button')].filter(b=>b.getClientRects().length).map(b=>({text:b.textContent,title:b.title,label:b.getAttribute('aria-label'),disabled:b.disabled}))})");
  await writeFile(resolve(root,"ui-failure.json"),JSON.stringify(diagnostics,null,2));
  throw Error(`Timed out: ${expression}\n${JSON.stringify(diagnostics)}`);
}
const click=name=>waitFor(`(()=>{const b=[...document.querySelectorAll('button')].find(b=>b.getClientRects().length&&!b.disabled&&(b.title===${JSON.stringify(name)}||b.getAttribute('aria-label')===${JSON.stringify(name)}||( ${JSON.stringify(name)}==='Run'?b.textContent.trim()==='Run':b.textContent.trim().startsWith(${JSON.stringify(name)}))));if(!b)return false;b.click();return true;})()`);
async function setValue(selector,value){
  await waitFor(`Boolean(document.querySelector(${JSON.stringify(selector+":not(:disabled)")}))`);
  if (await view.evaluate(`(()=>{const e=document.querySelector(${JSON.stringify(selector)});return e.tagName==='SELECT'&&e.value===${JSON.stringify(value)};})()`)) return;
  await view.evaluate(`(()=>{const e=document.querySelector(${JSON.stringify(selector)});const p=e.tagName==='SELECT'?HTMLSelectElement.prototype:HTMLTextAreaElement.prototype;Object.getOwnPropertyDescriptor(p,'value').set.call(e,${JSON.stringify(value)});e.dispatchEvent(new Event(e.tagName==='SELECT'?'change':'input',{bubbles:true}));})()`);
  await pause();
}
const settled=()=>waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.phase==='waiting_input')");
try{
  await boot();
  await view.evaluate(`(async()=>{const invoke=window.__TAURI_INTERNALS__.invoke;
    const s=await invoke('settings_snapshot');await invoke('settings_commit',{command:{commandId:'images.provider',expectedVersion:s.version,appearance:'system',portableHistoryEnabled:false,provider:{baseUrl:${JSON.stringify(origin+"/v1")},model:'vision-fixture',credentialAction:'keep',apiKey:null}}});
    const v=await invoke('settings_v2_snapshot');for(const p of v.settings.providers)for(const m of p.models)m.capabilities=['text','tools','vision'];
    // Keep the bundled default workflow ready; the Agent below binds only the two image tools.
    for(const t of v.settings.tools)t.enabled=true;
    v.settings.approvals.defaultMode='ask_for_approval';
    v.settings.projects=[{id:'project.images',name:'Image fixture',workspace:{kind:'local_directory',location:${JSON.stringify(project)}},defaultWorkflowId:'workflow.simple-chat',portableHistoryEnabled:false}];
    await invoke('settings_v2_commit',{command:{commandId:'images.tools',expectedVersion:v.version,settings:v.settings}});
    const w=await invoke('workflow_snapshot',{workflowId:'workflow.simple-chat'});w.document.nodes.find(n=>n.type==='agent').configuration.toolIds=[];
    await invoke('workflow_commit',{command:{commandId:'images.workflow',expectedVersion:w.version,workflowId:'workflow.simple-chat',document:w.document}});
  })()`);
  await view.command("Page.reload");
  await click("Settings");await click("Tools");
  await waitFor("Boolean(document.getElementById('tool.screenshot-authorityMode'))");
  assert.ok(await view.evaluate("document.getElementById('tool.screenshot-authorityMode').title.includes('visible')"));
  await click("Back to Chat");await click("Workflows");
  await setValue(".workflow-library-bar select","workflow.simple-chat");
  await waitFor("document.querySelector('.workflow-library-bar select')?.value==='workflow.simple-chat'&&document.body.innerText.includes('Version 2')");
  await view.evaluate("[...document.querySelectorAll('[aria-label=\"Workflow nodes\"] button')].find(b=>b.textContent==='Agent').click()");
  for(const name of ["Image read","Screenshot"]){
    await waitFor(`Boolean(document.querySelector('input[title="Bind ${name} to this agent"]'))`);
    await view.evaluate(`document.querySelector('input[title="Bind ${name} to this agent"]').click()`);
  }
  await click("Validate");await waitFor("document.body.innerText.includes('Validation passed: this workflow document is executable.')");
  await view.screenshot(resolve(root,"workflow-validation.png"));
  await click("Save");
  await waitFor("window.__TAURI_INTERNALS__.invoke('workflow_snapshot',{workflowId:'workflow.simple-chat'}).then(w=>w.document.nodes.find(n=>n.type==='agent').configuration.toolIds.includes('tool.screenshot'))");
  await click("Run");
  await waitFor("!document.querySelector('.workflow-library-bar')?.getClientRects().length && Boolean(document.querySelector('textarea[aria-label=\"Chat input\"]')?.getClientRects().length)");
  // Both image capabilities are usable without a saved project.
  const selectedProject = "";
  await waitFor(`Array.from(document.querySelector('select[aria-label="Project for the first Chat input"]').options).some(o=>o.value===${JSON.stringify(selectedProject)})`);
  await setValue('select[aria-label="Project for the first Chat input"]',selectedProject);
  await waitFor(`document.querySelector('select[aria-label="Project for the first Chat input"]').value===${JSON.stringify(selectedProject)}`);
  await setValue('select[aria-label="Workflow for the first Chat input"]',"workflow.simple-chat");
  await waitFor("document.querySelector('select[aria-label=\"Workflow for the first Chat input\"]')?.value==='workflow.simple-chat'");
  await setValue('textarea[aria-label="Chat input"]',localPaths ? `Tell me what you see in this image: "${externalPath}"` : `Read "${resolve(project,"red.png")}", then list screenshot targets and capture this Aworkit window.`);
  await click("Send");
  for(let i=0;i<(localPaths ? 0 : 2);i++){
    await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.phase==='awaiting_approval')");
    if(i===1)await view.screenshot(resolve(root,"capture-approval.png"));
    await click("Approve once");
    await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.events.filter(e=>e.kind==='approval.requested').length>=${i+2}||s.chat.phase==='waiting_input')`);
  }
  await settled();assert.deepEqual(failures,[]);assert.equal(requests.length,toolTurns+1);
  await waitFor(`document.querySelectorAll('.chat-image-list img').length===${imageCount}`);
  const previewLabel = `Preview ${localPaths ? imageName : 'screenshot.png'}`;
  await view.evaluate(`document.querySelector('button[aria-label=${JSON.stringify(previewLabel)}]').scrollIntoView({block:'center'})`);
  await click(previewLabel);
  await waitFor("document.querySelector('.chat-image-dialog img')?.naturalWidth>0");
  await view.screenshot(resolve(root,'expanded-preview.png'));
  await click('Close image preview');
  await view.screenshot(resolve(root,"chat.png"));
  await stop();await boot();await settled();
  await waitFor(`document.querySelectorAll('.chat-image-list img').length===${imageCount}`);
  await setValue('textarea[aria-label="Chat input"]',"Check the same saved images again.");
  await click("Queue");
  await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.phase==='waiting_input'&&s.events.filter(e=>e.kind==='message.user').length===2)");
  assert.deepEqual(failures,[]);assert.equal(requests.length,toolTurns+2);
  const snapshot=await view.evaluate("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0})");
  assert.equal(snapshot.events.filter(e=>e.kind==="approval.requested").length,localPaths ? 0 : 2);
  if (localPaths) {
    assert.equal(snapshot.events.find(e=>e.kind==='chat.started').payload.projectId, null);
    assert.ok(!JSON.stringify(snapshot.events).includes(imageData.split(',')[1]), "image bytes stay out of history");
    if (process.env.AWORKIT_QA_SOURCE_IMAGE) assert.equal(hash(await readFile(process.env.AWORKIT_QA_SOURCE_IMAGE)), sourceHash);
  }
  assert.ok(!JSON.stringify(snapshot.events).includes(png),"history contains references only");
  await view.screenshot(resolve(root,"reopened-chat.png"));
  const report={ok:true,root,requests:requests.length,...(localPaths?{prepared,modelBytes:Buffer.from(imageData.split(',')[1],'base64').length}:{}),cases:localPaths?["native editor binding","no project selected","invalid path arguments recover without failing the agent","absolute local image outside workspace","oversized source converted within model limit","original file unchanged","clickable preview","restart and follow-up preserve saved image after source deletion"]:["native tool Settings and editor binding","vision image bytes","source deletion","monitor and window enumeration","selected native window capture","two explicit screenshot approvals","clickable previews","restart and follow-up retain exact pixels without recapture"]};
  await writeFile(resolve(root,"result.json"),JSON.stringify(report,null,2));console.log(JSON.stringify(report));
}finally{
  await writeFile(resolve(root,"requests.json"),JSON.stringify(requests,null,2));
  await writeFile(resolve(root,"failures.json"),JSON.stringify(failures));
  await writeFile(resolve(root,"startup.log"),startupLog.join(''));
  await stop();provider.close();
}
