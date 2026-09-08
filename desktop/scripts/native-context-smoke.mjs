// Actual Windows WebView + trusted runtime + loopback provider, in an isolated profile.
// Run after rebuilding desktop/dist and the debug executable.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { copyFile, mkdir, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { resolve } from "node:path";
import { connectNativeWebView } from "./native-webview.mjs";

const root = resolve(`src-tauri/target/native-context-${Date.now()}`);
const project = resolve(root, "project");
await mkdir(project, { recursive: true });
await writeFile(resolve(project, "context-fixture.txt"), "ORIGINAL TOOL RESULT");
const executable = resolve(root, "aworkit-desktop.exe");
await copyFile(resolve(process.env.AWORKIT_QA_BINARY ?? "src-tauri/target/debug/aworkit-desktop.exe"), executable);
const requests = [], failures = [];
let releaseFirst;
const firstGate = new Promise(resolve => { releaseFirst = resolve; });
const provider = createServer(async (request, response) => {
  if (request.method !== "POST") {
    response.setHeader("Content-Type", "application/json");
    return response.end(JSON.stringify({ data: [{ id: "context-fixture" }] }));
  }
  try {
    let raw = "";
    for await (const chunk of request) raw += chunk;
    const body = JSON.parse(raw);
    requests.push(body);
    const turn = requests.length;
    let message;
    if (turn === 1) {
      await firstGate;
      const tool = body.tools.find(t => t.function.parameters.properties.path);
      assert.ok(tool, "frozen file-read definition was supplied");
      message = { tool_calls: [{ index: 0, id: "context.read.1", type: "function", function: { name: tool.function.name, arguments: JSON.stringify({ path: "context-fixture.txt" }) } }] };
    } else if (turn === 2) {
      assert.ok(body.messages.some(m => m.role === "tool" && m.content.includes("ORIGINAL TOOL RESULT")));
      message = { content: "ORIGINAL ASSISTANT RESPONSE" };
    } else if (turn === 3) {
      assert.ok(body.messages.some(m => m.role === "system" && m.content.includes("CONTEXT OVERRIDE ACTIVE")));
      const tool = body.messages.findIndex(m => m.role === "tool" && m.content.includes("EDITED TOOL RESULT"));
      const answer = body.messages.findIndex(m => m.role === "assistant" && m.content === "EDITED ASSISTANT RESPONSE");
      const follow = body.messages.findIndex(m => m.role === "user" && m.content === "Follow up after edit");
      assert.ok(tool >= 0 && answer > tool && follow > answer, "edited exchanges and response precede the follow-up input");
      message = { content: "Follow-up confirmed." };
    } else if (turn === 4) {
      assert.ok(!JSON.stringify(body).includes("CONTEXT OVERRIDE ACTIVE"), "new Chat does not inherit another Chat's edit");
      assert.ok(!body.tools?.length, "text-only workflow sends no tools");
      message = { content: "Fresh Chat confirmed." };
    } else {
      assert.ok(body.messages.some(m => m.role === "system" && m.content.includes("TEXT CONTEXT EDIT")));
      assert.equal(body.messages.at(-1).content, "Text-only follow up");
      message = { content: "Text-only edit confirmed." };
    }
    response.setHeader("Content-Type", "text/event-stream");
    response.end(
      `data: ${JSON.stringify({ choices: [{ index: 0, delta: message, finish_reason: null }] })}\n\n` +
      `data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: message.tool_calls ? "tool_calls" : "stop" }], usage: { prompt_tokens: 1000, completion_tokens: 24 } })}\n\n` +
      "data: [DONE]\n\n");
  } catch (error) {
    failures.push(String(error)); response.statusCode = 500; response.end(String(error));
  }
});
provider.listen(0, "127.0.0.1"); await once(provider, "listening");
const origin = `http://127.0.0.1:${provider.address().port}`;
const cdpPort = 9271;
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
  throw new Error("Timed out: " + expression + "\n" + await view.evaluate("document.body.innerText"));
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
const settled = () => waitFor("Boolean(document.querySelector('.composer-input .primary-action:not(:disabled)'))");
const panel = () => click(".context-ring-button").then(() => click(".context-display-button")).then(() => waitFor("Boolean(document.querySelector('.context-panel[open]'))"));
const closePanel = () => click('button[aria-label="Close context"]');
async function escape() {
  for (const type of ["keyDown", "keyUp"]) await view.command("Input.dispatchKeyEvent", { type, key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
}

try {
  await start();
  await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke)");
  await view.evaluate(`(async () => {
    const invoke=window.__TAURI_INTERNALS__.invoke;
    const s=await invoke('settings_snapshot');
    await invoke('settings_commit',{command:{commandId:'context.provider',expectedVersion:s.version,appearance:'light',portableHistoryEnabled:false,provider:{baseUrl:${JSON.stringify(origin + "/v1")},model:'context-fixture',credentialAction:'keep',apiKey:null}}});
    const v=await invoke('settings_v2_snapshot');
    for(const p of v.settings.providers)for(const m of p.models){m.capabilities=['text','tools'];m.contextWindow=2500;}
    v.settings.tools.find(t=>t.id==='tool.files.read').enabled=true;
    v.settings.projects=[{id:'project.context',name:'Context fixture',workspace:{kind:'local_directory',location:${JSON.stringify(project)}},defaultWorkflowId:'workflow.simple-chat',portableHistoryEnabled:false}];
    await invoke('settings_v2_commit',{command:{commandId:'context.tools',expectedVersion:v.version,settings:v.settings}});
    const w=await invoke('workflow_snapshot',{workflowId:'workflow.simple-chat'});
    w.document.nodes.find(n=>n.type==='agent').configuration.toolIds=['tool.files.read'];
    await invoke('workflow_commit',{command:{commandId:'context.workflow',expectedVersion:w.version,workflowId:'workflow.simple-chat',document:w.document}});
    await invoke('workflow_set_default',{command:{commandId:'context.default',workflowId:'workflow.simple-chat'}});
  })()`);
  await view.command("Page.reload");
  await setValue('select[aria-label="Workflow for the first Chat input"]', "workflow.simple-chat");
  await setValue('select[aria-label="Project for the first Chat input"]', "project.context");
  await setValue('textarea[aria-label="Chat input"]', "Read context-fixture.txt");
  await click(".composer-input .primary-action");
  await waitFor("Boolean(document.querySelector('.model-call-block'))");
  await panel();
  assert.equal(await view.evaluate("[...document.querySelectorAll('.context-panel button')].find(b=>b.textContent==='Enable Edit').disabled"), true);
  await closePanel();
  releaseFirst();
  await setValue('textarea[aria-label="Chat input"]', "Follow up after edit");
  await settled();
  assert.ok(await view.evaluate("(() => { const input=document.querySelector('.composer-input').getBoundingClientRect();const send=document.querySelector('.composer-input .primary-action').getBoundingClientRect();const ring=document.querySelector('.context-ring-button').getBoundingClientRect();return send.left>ring.right && send.bottom<=input.bottom && Math.abs(send.bottom-ring.bottom)<4; })()"), "ring and send stay on the same composer row");
  const before = await snapshot();
  assert.equal(before.chat.phase, "waiting_input");
  assert.equal(requests.length, 2);
  await click(".context-ring-button");
  await waitFor("document.querySelector('.context-popover')?.textContent.includes('41%')");
  await view.screenshot(resolve(root, "usage-popup.png"));
  await click(".context-display-button");
  await waitFor("Boolean(document.querySelector('.context-panel[open]'))");
  assert.equal(await view.evaluate("document.querySelector('.context-source').readOnly"), true);
  await closePanel();
  assert.equal((await snapshot()).events.filter(e => e.kind === "context.edited").length, 0);
  await panel();
  await click('.context-panel-toolbar button');
  const original = await view.evaluate("JSON.parse(document.querySelector('.context-source').value)");
  assert.equal(original.exchanges.length, 1);
  await setValue(".context-source", "{");
  await escape();
  await waitFor("Boolean(document.querySelector('.context-panel-error'))");
  const edited = structuredClone(original);
  edited.input.messages[0].content += "\nCONTEXT OVERRIDE ACTIVE";
  edited.exchanges[0].results[0].content = "EDITED TOOL RESULT";
  edited.contextMessages.find(m => m.role === "assistant").content = "EDITED ASSISTANT RESPONSE";
  await setValue(".context-source", JSON.stringify(edited, null, 2));
  await view.screenshot(resolve(root, "editable-context.png"));
  await closePanel();
  await waitFor("!document.querySelector('.context-panel')");
  const saved = await snapshot();
  const edits = saved.events.filter(e => e.kind === "context.edited");
  assert.equal(edits.length, 1);
  assert.ok(edits[0].payload.beforeHash && edits[0].payload.afterHash);
  assert.equal(saved.events.filter(e => e.kind === "message.assistant")[0].payload.body, "ORIGINAL ASSISTANT RESPONSE");
  await view.evaluate("document.querySelector('.timeline-scroll').scrollTop=1e8");
  await waitFor("document.querySelector('.timeline-scroll')?.textContent.includes('Context edited')");
  await view.screenshot(resolve(root, "context-event.png"));
  await writeFile(resolve(project, "context-fixture.txt"), "CHANGED ON DISK");
  // Restart before sending: edits must be restored from durable events.
  await stop(); await start();
  await setValue('textarea[aria-label="Chat input"]', "Follow up after edit");
  await panel();
  assert.ok(await view.evaluate("document.querySelector('.context-source').value.includes('EDITED TOOL RESULT')"));
  await closePanel();
  await click(".composer-input .primary-action");
  await waitFor("document.querySelector('.timeline-scroll')?.textContent.includes('Follow-up confirmed.')");
  const continued = await snapshot();
  assert.equal(requests.length, 3);
  assert.equal(continued.events.filter(e => e.kind === "span.started" && e.payload.spanKind === "tool_call").length, 1, "historical tool call was not executed again");
  assert.equal(continued.events.filter(e => e.kind === "context.edited").length, 1);
  // Fresh Chat isolation, through the actual sidebar action.
  await view.evaluate(`(async()=>{const invoke=window.__TAURI_INTERNALS__.invoke;
    const w=await invoke('workflow_snapshot',{workflowId:'workflow.simple-chat'});
    w.document.nodes.find(n=>n.type==='agent').configuration.toolIds=[];
    await invoke('workflow_commit',{command:{commandId:'context.text-workflow',expectedVersion:w.version,workflowId:'workflow.simple-chat',document:w.document}});
    const s=await invoke('settings_v2_snapshot');s.settings.appearance.mode='dark';
    await invoke('settings_v2_commit',{command:{commandId:'context.dark',expectedVersion:s.version,settings:s.settings}});
  })()`);
  await view.command("Page.reload");
  await click("button.new-chat");
  await setValue('select[aria-label="Workflow for the first Chat input"]', "workflow.simple-chat");
  await setValue('select[aria-label="Project for the first Chat input"]', "project.context");
  await setValue('textarea[aria-label="Chat input"]', "Fresh Chat");
  await click(".composer-input .primary-action");
  await waitFor("document.querySelector('.timeline-scroll')?.textContent.includes('Fresh Chat confirmed.')");
  assert.equal(requests.length, 4);
  await view.command("Emulation.setDeviceMetricsOverride", { width: 1000, height: 740, deviceScaleFactor: 1, mobile: false });
  // WebView2 retains native pointer scaling under viewport emulation. The
  // un-emulated path above tests pointer delivery; here exercise layout/handlers.
  await view.evaluate("document.querySelector('.context-ring-button').click()");
  await waitFor("Boolean(document.querySelector('.context-display-button:not(:disabled)'))");
  await view.evaluate("document.querySelector('.context-display-button').click()");
  await waitFor("Boolean(document.querySelector('.context-panel[open]'))");
  await waitFor("document.documentElement.dataset.appearance==='dark'");
  assert.ok(await view.evaluate("(() => { const r=document.querySelector('.context-panel').getBoundingClientRect();const f=document.querySelector('.context-panel-footer').getBoundingClientRect();return r.left>=0 && r.top>=0 && r.right<=innerWidth && r.bottom<=innerHeight && f.bottom<=innerHeight; })()"), "context panel and close controls fit a smaller native viewport");
  await view.screenshot(resolve(root, "context-dark-small.png"));
  await view.evaluate("document.querySelector('.context-panel-toolbar button').click()");
  const textDocument = await view.evaluate("JSON.parse(document.querySelector('.context-source').value)");
  textDocument.input.messages[0].content += "\nTEXT CONTEXT EDIT";
  await setValue(".context-source", JSON.stringify(textDocument, null, 2));
  await escape();
  await waitFor("!document.querySelector('.context-panel')");
  await view.command("Emulation.clearDeviceMetricsOverride");
  await setValue('textarea[aria-label="Chat input"]', "Text-only follow up");
  await click(".composer-input .primary-action");
  await waitFor("document.querySelector('.timeline-scroll')?.textContent.includes('Text-only edit confirmed.')");
  assert.equal(requests.length, 5);
  assert.deepEqual(failures, []);
  const report = { ok: true, root, providerRequests: requests.length, cases: [
    "live ring and active-edit lock", "41 percent provider usage popup", "read-only raw viewer",
    "invalid JSON Escape preserves draft", "close commits exactly one visible event",
    "original Chat evidence unchanged", "restart restores edited system, tool result and answer",
    "next native provider request uses edits in order", "historical tools are not replayed", "Chat isolation",
    "dark smaller viewport", "text-only context edit via Escape close",
  ] };
  await writeFile(resolve(root, "report.json"), JSON.stringify(report, null, 2));
  await writeFile(resolve(root, "requests.json"), JSON.stringify(requests, null, 2));
  console.log(JSON.stringify(report, null, 2));
} finally {
  releaseFirst(); await stop();
  await writeFile(resolve(root, "native.log"), logs);
  provider.closeAllConnections(); provider.close();
}
