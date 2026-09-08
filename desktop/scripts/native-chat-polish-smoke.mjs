// Real WebView2 + native runtime proof for composer controls, resizing, streaming and metadata.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { copyFile, mkdir, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { resolve } from "node:path";
import { connectNativeWebView } from "./native-webview.mjs";

const root = resolve(`src-tauri/target/native-chat-polish-${Date.now()}`);
const project = resolve(root, "project");
await mkdir(project, { recursive: true });
await writeFile(resolve(project, "AGENTS.md"), "POLISH WORKSPACE INSTRUCTIONS");
const executable = resolve(root, "aworkit-desktop.exe");
await copyFile(resolve(process.env.AWORKIT_QA_BINARY ?? "src-tauri/target/debug/aworkit-desktop.exe"), executable);
const requests = [], responses = [];
let catalogs = 0, child, view, logs = "";
const provider = createServer(async (request, response) => {
  if (request.method !== "POST") {
    catalogs++;
    response.setHeader("Content-Type", "application/json");
    return response.end(JSON.stringify({ data: [{ id: "polish-fixture", max_model_len: 32768 }] }));
  }
  let raw = "";
  for await (const chunk of request) raw += chunk;
  requests.push(JSON.parse(raw));
  responses.push(response);
  response.setHeader("Content-Type", "text/event-stream");
  response.flushHeaders();
});
provider.listen(0, "127.0.0.1"); await once(provider, "listening");
const origin = `http://127.0.0.1:${provider.address().port}/v1`;
const pause = (ms = 100) => new Promise(resolve => setTimeout(resolve, ms));
const emit = (text, index = 0) => responses[index].write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { content: text }, finish_reason: null }] })}\n\n`);
const finish = (index = 0) => responses[index].end(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: "stop" }], usage: { prompt_tokens: 1234, completion_tokens: 56 } })}\n\ndata: [DONE]\n\n`);
async function waitFor(expression) {
  for (let n = 0; n < 200; n++) {
    if (await view.evaluate(expression)) return;
    await pause();
  }
  throw new Error("Timed out: " + expression + "\n" + await view.evaluate("document.body.innerText"));
}
async function point(selector) {
  return view.evaluate(`(() => {const e=document.querySelector(${JSON.stringify(selector)}); const r=e.getBoundingClientRect();return {x:r.left+r.width/2,y:r.top+r.height/2};})()`);
}
async function click(selector) {
  await waitFor(`Boolean(document.querySelector(${JSON.stringify(selector)} + ':not(:disabled)'))`);
  const at = await point(selector);
  for (const type of ["mousePressed", "mouseReleased"]) await view.command("Input.dispatchMouseEvent", { type, ...at, button: "left", clickCount: 1 });
}
async function value(selector, value) {
  await view.evaluate(`(() => {const e=document.querySelector(${JSON.stringify(selector)});const proto=e.tagName==='SELECT'?HTMLSelectElement.prototype:HTMLTextAreaElement.prototype;Object.getOwnPropertyDescriptor(proto,'value').set.call(e,${JSON.stringify(value)});e.dispatchEvent(new Event(e.tagName==='SELECT'?'change':'input',{bubbles:true}));})()`);
}
async function drag(selector, delta) {
  const at = await point(selector);
  await view.command("Input.dispatchMouseEvent", { type: "mousePressed", ...at, button: "left", clickCount: 1 });
  await view.command("Input.dispatchMouseEvent", { type: "mouseMoved", x: at.x + delta, y: at.y, button: "left", buttons: 1 });
  await view.command("Input.dispatchMouseEvent", { type: "mouseReleased", x: at.x + delta, y: at.y, button: "left", clickCount: 1 });
  await pause(200);
}
const scrollState = () => view.evaluate(`(() => { const e=document.querySelector('.timeline-scroll');return {top:e.scrollTop,gap:e.scrollHeight-e.scrollTop-e.clientHeight,following:e.dataset.followLatest,height:e.scrollHeight}; })()`);
try {
  child = spawn(executable, [], { windowsHide: true, stdio: ["ignore", "pipe", "pipe"], env: {
    ...process.env, AWORKIT_QA_PROFILE: root, AWORKIT_QA_HIDE_WINDOW: "1",
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: "--remote-debugging-port=9286",
  } });
  child.stdout.on("data", chunk => { logs += chunk; }); child.stderr.on("data", chunk => { logs += chunk; });
  for (let n = 0; n < 150; n++) { try { view = await connectNativeWebView("http://127.0.0.1:9286"); break; } catch { await pause(); } }
  assert.ok(view, logs);
  await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke)");
  await view.command("Emulation.setDeviceMetricsOverride", { width: 1600, height: 1000, deviceScaleFactor: 1, mobile: false });
  await view.evaluate(`(async () => {
    const invoke=window.__TAURI_INTERNALS__.invoke;
    const s=await invoke('settings_snapshot');
    await invoke('settings_commit',{command:{commandId:'polish.provider',expectedVersion:s.version,appearance:'light',portableHistoryEnabled:false,provider:{baseUrl:${JSON.stringify(origin)},model:'polish-fixture',credentialAction:'keep',apiKey:null}}});
    const v=await invoke('settings_v2_snapshot');
    for(const p of v.settings.providers)for(const m of p.models){m.capabilities=['text','tools'];m.contextWindow=null;}
    const t=v.settings.tools.find(t=>t.id==='tool.workspace_instructions');t.enabled=true;t.configuration.aworkitHome=${JSON.stringify(resolve(root, "global"))};
    v.settings.projects=[{id:'project.polish',name:'Composer verification',workspace:{kind:'local_directory',location:${JSON.stringify(project)}},defaultWorkflowId:'workflow.simple-chat',portableHistoryEnabled:false}];
    await invoke('settings_v2_commit',{command:{commandId:'polish.tools',expectedVersion:v.version,settings:v.settings}});
    const w=await invoke('workflow_snapshot',{workflowId:'workflow.simple-chat'});
    w.document.nodes.find(n=>n.type==='agent').configuration.toolIds=['tool.workspace_instructions'];
    await invoke('workflow_commit',{command:{commandId:'polish.workflow',expectedVersion:w.version,workflowId:'workflow.simple-chat',document:w.document}});
    await invoke('workflow_set_default',{command:{commandId:'polish.default',workflowId:'workflow.simple-chat'}});
  })()`);
  await view.command("Page.reload");
  await waitFor("Boolean(document.querySelector('.composer-submit'))");
  await value('select[aria-label="Workflow for the first Chat input"]', "workflow.simple-chat");
  await value('select[aria-label="Project for the first Chat input"]', "project.polish");
  await waitFor("/32[,.]768/.test(document.querySelector('.context-ring-button').title)");
  await drag('.desktop-shell > .pane-splitter', 180);
  await drag('.inspector-splitter', -200);
  const widths = await view.evaluate(`({nav:document.querySelector('.navigation-pane').getBoundingClientRect().width,details:document.querySelector('.run-details-inspector').getBoundingClientRect().width})`);
  assert.ok(widths.nav >= 380 && widths.details >= 510, JSON.stringify(widths));
  assert.equal(await view.evaluate("document.querySelectorAll('.run-actions button').length"), 1);
  assert.equal(await view.evaluate("document.querySelector('.approval-mode-select').parentElement.className"), "composer-toolbar");
  assert.equal(await view.evaluate("document.querySelector('.approval-mode-select > span') === null"), true);
  assert.equal(await view.evaluate("document.querySelector('.composer-footer').textContent.includes('Waiting for input')"), true);
  await value('textarea[aria-label="Chat input"]', "Stream a detailed response");
  await click('.composer-submit');
  for (let n = 0; n < 200 && !requests.length; n++) await pause();
  assert.equal(requests.length, 1);
  const clock = JSON.stringify(requests[0]).match(/Chat started: (\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ)/)?.[1];
  assert.ok(clock, "Chat clock reaches the real provider payload");
  assert.ok(JSON.stringify(requests[0]).includes("POLISH WORKSPACE INSTRUCTIONS"));
  await waitFor("document.querySelector('.composer-submit').getAttribute('aria-label')==='Stop response'");
  emit("## Streaming verification\n\n" + "First paragraph with enough text to wrap.\n\n".repeat(45));
  await waitFor("document.querySelector('.timeline-scroll').scrollHeight>1500");
  await pause(500);
  assert.ok((await scrollState()).gap < 5, JSON.stringify(await scrollState()));
  const at = await point('.timeline-scroll');
  await view.command("Input.dispatchMouseEvent", { type: "mouseWheel", ...at, deltaX: 0, deltaY: -550 });
  await pause(250);
  const held = await scrollState(); assert.equal(held.following, "false");
  emit("Second batch arrives while reading earlier text.\n\n".repeat(20));
  await pause(500);
  assert.ok(Math.abs((await scrollState()).top - held.top) < 4, "manual reading position survives new chunks");
  await view.command("Input.dispatchMouseEvent", { type: "mouseWheel", ...at, deltaX: 0, deltaY: 100000 });
  await pause(300);
  console.log("Returned to bottom", await scrollState());
  await waitFor("document.querySelector('.timeline-scroll').dataset.followLatest==='true'");
  emit("Last arriving paragraph.\n\n".repeat(20));
  await pause(500);
  assert.ok((await scrollState()).gap < 5, JSON.stringify(await scrollState()));
  const thumb = await view.evaluate(`(() => {const e=document.querySelector('.timeline-scroll'),r=e.getBoundingClientRect();return {x:r.right-7,y:r.bottom-16-(e.clientHeight-32)*e.clientHeight/e.scrollHeight/2};})()`);
  await view.command("Input.dispatchMouseEvent", { type: "mousePressed", ...thumb, button: "left", clickCount: 1 });
  await view.command("Input.dispatchMouseEvent", { type: "mouseMoved", x: thumb.x, y: thumb.y - 100, button: "left", buttons: 1 });
  await view.command("Input.dispatchMouseEvent", { type: "mouseReleased", x: thumb.x, y: thumb.y - 100, button: "left", clickCount: 1 });
  await pause(200);
  const dragged = await scrollState();
  assert.equal(dragged.following, "false", "dragging the scrollbar pauses following");
  emit("New text while scrollbar is deliberately above the bottom.\n\n".repeat(10));
  await pause(400);
  assert.ok(Math.abs((await scrollState()).top - dragged.top) < 4, "scrollbar reading position survives new content");
  await view.command("Input.dispatchMouseEvent", { type: "mouseWheel", ...at, deltaX: 0, deltaY: 100000 });
  await waitFor("document.querySelector('.timeline-scroll').dataset.followLatest==='true'");
  finish();
  await waitFor("document.querySelector('.composer-footer').textContent.includes('Waiting for input')");
  await click('.context-ring-button');
  await waitFor("/1[,.]290 \\/ 32[,.]768/.test(document.querySelector('.context-popover-summary').innerText)");
  await view.screenshot(resolve(root, "context-and-composer.png"));
  await click('.context-ring-button');
  await value('textarea[aria-label="Chat input"]', "Another response"); await click('.composer-submit');
  for (let n = 0; n < 200 && requests.length < 2; n++) await pause();
  assert.equal(requests.length, 2);
  assert.ok(JSON.stringify(requests[1]).includes(`Chat started: ${clock}`), "start time remains unchanged on follow-up");
  emit("Response that will be stopped", 1);
  await click('.composer-submit[aria-label="Stop response"]');
  // A streaming provider continues delivering bytes while cooperative cancellation settles.
  await pause(100); emit(" next streamed token", 1);
  await waitFor("document.querySelector('.composer-footer').textContent.includes('Waiting for input')");
  assert.ok(await view.evaluate("(async()=>{const s=await window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0});return s.events.some(e=>e.kind==='chat.turn_stopped');})()"));
  assert.equal(await view.evaluate("document.querySelector('.composer-submit').getAttribute('aria-label')"), "Queue");
  await writeFile(resolve(root, "result.json"), JSON.stringify({ widths, clock, catalogs, requests: requests.length, scroll: await scrollState(), passed: true }, null, 2));
  console.log("PASS", root);
} catch (error) {
  await view?.screenshot(resolve(root, "failure.png")).catch(() => {});
  await writeFile(resolve(root, "failure.txt"), String(error) + "\n" + logs);
  throw error;
} finally {
  view?.close();
  for (const response of responses) response.destroy();
  provider.closeAllConnections(); provider.close();
  if (child?.exitCode === null) { const exit = once(child, "exit"); child.kill(); await exit; }
}
