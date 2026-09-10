// Workspace Instructions through the real editor, native core and loopback provider.
// Isolated profile, project and provider; also exercises context replacement/restart.
import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { mkdir, writeFile, unlink } from "node:fs/promises";
import { resolve } from "node:path";
import { once } from "node:events";
import assert from "node:assert/strict";
import { connectNativeWebView } from "./native-webview.mjs";

const root = resolve(`src-tauri/target/native-instructions-${Date.now()}`);
const project = resolve(root, "project"), globalHome = resolve(root, "global");
await mkdir(resolve(project, "src"), { recursive: true });
await mkdir(resolve(project, ".git"));
await mkdir(globalHome);
await writeFile(resolve(project, ".git/HEAD"), "ref: refs/heads/main\n");
await writeFile(resolve(project, "AGENTS.md"), "PROJECT V1");
await writeFile(resolve(globalHome, "AGENTS.md"), "GLOBAL V1");
await writeFile(resolve(project, "src/AGENTS.md"), "NESTED V1");
await writeFile(resolve(project, "src/file.txt"), "file content");
const requests = [], failures = [];
let scenario = "tools", toolTurns = 0, childTurns = 0;
const provider = createServer(async (request, response) => {
  try {
    if (request.url === "/v1/models") return response.end(JSON.stringify({ data: [{ id: "instructions-fixture" }] }));
    const chunks = []; for await (const chunk of request) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks).toString());
    requests.push({ scenario, body });
    await writeFile(resolve(root, "requests.json"), JSON.stringify(requests, null, 2));
    const reminders = body.messages.filter(m => typeof m.content === "string" && m.content.includes("Instructions from:") || typeof m.content === "string" && m.content.includes("instructions from:") || typeof m.content === "string" && m.content.includes("Instructions removed:"));
    const guidance = reminders.map(m => m.content).join("\n");
    assert.ok(!(body.tools ?? []).some(t => /workspace_instructions/.test(t.function.name)));
    let call = null;
    if (scenario === "disabled") assert.equal(reminders.length, 0);
    else {
      assert.ok(guidance.includes("GLOBAL V1"), `${scenario}: global missing`);
      assert.ok(reminders.every(m => m.role === "user"));
      if (scenario === "tools") {
        if (toolTurns === 0) { assert.ok(guidance.includes("PROJECT V1")); assert.ok(!guidance.includes("NESTED V1")); }
        if (toolTurns === 1) {
          assert.ok(guidance.includes("NESTED V1"));
          assert.ok(body.messages.indexOf(reminders.at(-1)) > body.messages.findIndex(m => m.role === "tool"));
          await writeFile(resolve(project, "AGENTS.md"), "PROJECT V2");
        }
        if (toolTurns === 2) {
          assert.ok(guidance.includes("Updated instructions from: AGENTS.md")); assert.ok(guidance.includes("PROJECT V2"));
          await unlink(resolve(project, "src/AGENTS.md"));
        }
        if (toolTurns === 3) assert.ok(guidance.includes("Instructions removed: src/AGENTS.md"));
        if (toolTurns < 3) call = { id: `read.${toolTurns}`, type: "function", function: { name: "read_file", arguments: JSON.stringify({ path: "src/file.txt" }) } };
        toolTurns++;
      } else if (scenario === "follow-up") {
        assert.equal(reminders.filter(m => m.content.includes("The following workspace instructions")).length, 1);
        assert.ok(guidance.includes("PROJECT V2"));
        const firstHuman = body.messages.findIndex(m => m.content === "Run instruction checks.");
        assert.equal(body.messages.indexOf(reminders[0]), firstHuman + 1, "historical baseline retains its original position");
      } else if (scenario === "restored") {
        assert.equal(reminders.length, 1); assert.ok(guidance.includes("PROJECT V3")); assert.ok(guidance.includes("NESTED V2"));
        assert.ok(!guidance.includes("PROJECT V1"));
      } else if (scenario === "child") {
        assert.ok(guidance.includes("PROJECT V3"));
        assert.equal(reminders.length, 1, "parent and child own separate instruction state");
        if (childTurns === 0) call = { id: "delegate.1", type: "function", function: { name: "spawn_subagent", arguments: JSON.stringify({ task: "Read the provided instructions and report." }) } };
        if (childTurns === 1) assert.equal((body.tools ?? []).length, 0, "instruction-only child has no callable schemas");
        childTurns++;
      } else {
        assert.equal((body.tools ?? []).length, 0, "instruction-only Agent uses text model requests");
        assert.equal(reminders.length, 1); assert.ok(guidance.includes("PROJECT V3"));
      }
    }
    response.setHeader("Content-Type", "text/event-stream");
    const delta = call ? { role: "assistant", tool_calls: [{ index: 0, ...call }] } : { role: "assistant", content: `Instruction fixture ${scenario} complete.` };
    for (const chunk of [{ choices: [{ index: 0, delta, finish_reason: null }] }, { choices: [{ index: 0, delta: {}, finish_reason: call ? "tool_calls" : "stop" }] }, { choices: [], usage: { prompt_tokens: 20, completion_tokens: 10, total_tokens: 30 } }]) response.write(`data: ${JSON.stringify(chunk)}\n\n`);
    response.end("data: [DONE]\n\n");
  } catch (error) { failures.push(String(error)); response.statusCode = 500; response.end(String(error)); }
});
provider.listen(0, "127.0.0.1"); await once(provider, "listening");
const origin = `http://127.0.0.1:${provider.address().port}`, port = 9276;
const executable = process.env.AWORKIT_QA_EXECUTABLE ?? resolve("src-tauri/target/debug/aworkit-desktop.exe");
const launch = () => spawn(executable, [], { windowsHide: true, stdio: "ignore", env: { ...process.env, AWORKIT_QA_PROFILE: root, AWORKIT_QA_HIDE_WINDOW: "1", WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}` } });
let child = launch(), view;
const pause = () => new Promise(resolve => setTimeout(resolve, 100));
async function connect() { for (let i = 0; i < 150; i++) { try { return await connectNativeWebView(`http://127.0.0.1:${port}`); } catch {} await pause(); } throw new Error("Native WebView did not start"); }
async function waitFor(expression) {
  for (let i = 0; i < 250; i++) { if (await view.evaluate(expression)) return; if (failures.length) throw new Error(failures.join("\n")); await pause(); }
  throw new Error(`Timed out: ${expression}\n${await view.evaluate("document.body.innerText")}`);
}
const click = name => waitFor(`(() => { const b=[...document.querySelectorAll('button,[role="button"]')].find(b=>!b.disabled&&b.offsetParent!==null&&(b.title===${JSON.stringify(name)}||b.getAttribute('aria-label')===${JSON.stringify(name)}||b.textContent.trim().replace(/^[＋⚙◇○]\\s*/,'').startsWith(${JSON.stringify(name)}))); if(!b)return false;b.click();return true;})()`);
async function setValue(selector, value) {
  await waitFor(`Boolean(document.querySelector(${JSON.stringify(selector + ":not(:disabled)")}))`);
  await view.evaluate(`(() => {const e=document.querySelector(${JSON.stringify(selector)});const p=e.tagName==='SELECT'?HTMLSelectElement.prototype:HTMLTextAreaElement.prototype;Object.getOwnPropertyDescriptor(p,'value').set.call(e,${JSON.stringify(value)});e.dispatchEvent(new Event(e.tagName==='SELECT'?'change':'input',{bubbles:true}));})()`);
}
async function send(text) {
  const before = requests.length;
  await waitFor("Boolean(document.querySelector('textarea[aria-label=\"Chat input\"]')?.offsetParent)");
  await setValue('textarea[aria-label="Chat input"]', text);
  const locked = await view.evaluate("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.lockedWorkflow)");
  await click(locked ? "Queue" : "Send");
  await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.phase==='waiting_input')");
  assert.ok(requests.length > before); assert.deepEqual(failures, []);
}
async function close() { view?.close(); view = undefined; if (child.exitCode === null) { const exited = once(child, "exit"); child.kill(); await exited; } }
async function workflowTools(ids, capabilities) {
  await view.evaluate(`(async()=>{const i=window.__TAURI_INTERNALS__.invoke; const v=await i('settings_v2_snapshot'); for(const p of v.settings.providers)for(const m of p.models)m.capabilities=${JSON.stringify(capabilities)};
    if(${JSON.stringify(ids)}.includes('tool.subagent')) {
      for(const t of v.settings.tools)t.enabled=${JSON.stringify(ids)}.includes(t.id);
      v.settings.tools.find(t=>t.id==='tool.subagent').options={approvalMode:'full_access'};
    }
    await i('settings_v2_commit',{command:{commandId:crypto.randomUUID(),expectedVersion:v.version,settings:v.settings}});
    const w=await i('workflow_snapshot',{workflowId:'workflow.simple-chat'});w.document.nodes.find(n=>n.type==='agent').configuration.toolIds=${JSON.stringify(ids)};
    await i('workflow_commit',{command:{commandId:crypto.randomUUID(),expectedVersion:w.version,workflowId:'workflow.simple-chat',document:w.document}});
    await i('workflow_set_default',{command:{commandId:crypto.randomUUID(),workflowId:'workflow.simple-chat'}});})()`);
  // Fixture API edits bypass the editor's local invalidation; reload its caches.
  await view.command("Page.reload");
  await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke)");
}
async function newChat() {
  const previous = await view.evaluate("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId)");
  await click("New Chat");
  await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId!==${JSON.stringify(previous)}&&s.chat.phase==='draft')`);
  await waitFor("Boolean(document.querySelector('textarea[aria-label=\"Chat input\"]')?.offsetParent)");
  await setValue('select[aria-label="Project for the first Chat input"]', "project.instructions");
  await setValue('select[aria-label="Workflow for the first Chat input"]', "workflow.simple-chat");
}
try {
  view = await connect();
  await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke)");
  await view.evaluate(`(async()=>{const i=window.__TAURI_INTERNALS__.invoke;
    const s=await i('settings_snapshot');await i('settings_commit',{command:{commandId:'instructions.provider',expectedVersion:s.version,appearance:'system',portableHistoryEnabled:false,provider:{baseUrl:${JSON.stringify(origin + "/v1")},model:'instructions-fixture',credentialAction:'keep',apiKey:null}}});
    const v=await i('settings_v2_snapshot');for(const p of v.settings.providers)for(const m of p.models)m.capabilities=['text','tools'];
    for(const t of v.settings.tools)t.enabled=true;
    const t=v.settings.tools.find(t=>t.id==='tool.workspace_instructions');t.enabled=false;t.configuration.aworkitHome=${JSON.stringify(globalHome)};
    v.settings.projects=[{id:'project.instructions',name:'Instructions fixture',workspace:{kind:'local_directory',location:${JSON.stringify(project)}},defaultWorkflowId:'workflow.simple-chat',portableHistoryEnabled:false}];
    await i('settings_v2_commit',{command:{commandId:'instructions.settings',expectedVersion:v.version,settings:v.settings}});
    const w=await i('workflow_snapshot',{workflowId:'workflow.simple-chat'});w.document.nodes.find(n=>n.type==='agent').configuration.toolIds=[];
    await i('workflow_commit',{command:{commandId:'instructions.workflow',expectedVersion:w.version,workflowId:'workflow.simple-chat',document:w.document}});})()`);
  await view.command("Page.reload"); await click("Settings"); await click("Tools");
  await waitFor("Boolean(document.getElementById('tool.workspace_instructions-enabled'))");
  await view.evaluate("document.getElementById('tool.workspace_instructions-enabled').click()");
  await click("Save configuration");
  await waitFor("window.__TAURI_INTERNALS__.invoke('settings_v2_snapshot').then(s=>s.settings.tools.find(t=>t.id==='tool.workspace_instructions').enabled)");
  assert.ok(await view.evaluate("document.getElementById('tool.workspace_instructions-maxBytes').title.length>10"));
  await view.evaluate("document.getElementById('tool.workspace_instructions-enabled').closest('.settings-record').scrollIntoView({block:'start'})");
  await view.screenshot(resolve(root, "settings.png"));
  await click("Back to Chat"); await click("Workflows"); await setValue('.workflow-library-bar select', "workflow.simple-chat");
  await waitFor("document.querySelector('.workflow-library-bar select')?.value==='workflow.simple-chat' && document.body.innerText.includes('Version 2')");
  await view.evaluate("[...document.querySelectorAll('[aria-label=\"Workflow nodes\"] button')].find(b=>b.textContent==='Agent').click()");
  for (const name of ["Workspace Instructions", "Project file read"]) {
    await waitFor(`(()=>{const e=document.querySelector('input[title="Bind ${name} to this agent"]');if(!e)return false;if(!e.checked)e.click();return true;})()`);
  }
  await click("Validate"); await waitFor("document.body.innerText.includes('Validation passed: this workflow document is executable.')");
  await view.evaluate("document.querySelector('input[title=\"Bind Workspace Instructions to this agent\"]').scrollIntoView({block:'center'})");
  await view.screenshot(resolve(root, "workflow-validation.png")); await click("Save"); await click("Run");
  await waitFor("Boolean(document.querySelector('textarea[aria-label=\"Chat input\"]')?.offsetParent)");
  await setValue('select[aria-label="Project for the first Chat input"]', "project.instructions");
  await setValue('select[aria-label="Workflow for the first Chat input"]', "workflow.simple-chat");
  await send("Run instruction checks."); assert.equal(toolTurns, 4);
  scenario = "follow-up"; await send("Continue in this Chat.");
  // Discover a reappearing nested rule before removing the visible projection.
  await writeFile(resolve(project, "src/AGENTS.md"), "NESTED V2");
  await send("Observe offline changes.");
  await writeFile(resolve(project, "AGENTS.md"), "PROJECT V3");
  await view.evaluate(`(async()=>{const i=window.__TAURI_INTERNALS__.invoke;const s=await i('desktop_snapshot',{afterSequence:0});
    const model=s.events.filter(e=>e.kind==='span.started'&&e.payload.spanKind==='model_call').at(-1);
    const done=s.events.find(e=>e.kind==='span.completed'&&e.spanId===model.spanId);
    const raw=model.payload.input;const document={input:{messages:[{role:'user',content:'Summary of earlier work. Instruction events were compacted out.'}]},tools:raw.tools??[],exchanges:[],contextMessages:[]};
    const result=await i('desktop_command',{command:{schemaVersion:1,commandId:crypto.randomUUID(),expectedVersion:s.version,action:'edit_context',targetId:s.chat.chatId,payload:{nodeId:'agent.1',baseSequence:done?.sequence??model.sequence,document}}});if(!result.accepted)throw Error(result.reason);})()`);
  await close(); child = launch(); view = await connect();
  scenario = "restored"; await send("Continue after restart and context replacement.");
  await workflowTools(["tool.workspace_instructions"], ["text"]); await newChat();
  scenario = "text-only"; await send("Use global and project instructions with a text-only model.");
  await workflowTools(["tool.workspace_instructions", "tool.subagent"], ["text", "tools"]); await newChat();
  scenario = "child"; await send("Delegate to an instruction-only child."); assert.equal(childTurns, 3);
  await workflowTools([], ["text"]); await newChat(); scenario = "disabled"; await send("No automatic instructions in this Agent.");
  await view.screenshot(resolve(root, "completed.png"));
  console.log(JSON.stringify({ ok: true, root, requests: requests.length, scenarios: [...new Set(requests.map(r=>r.scenario))] }));
} finally { await close(); provider.close(); }
