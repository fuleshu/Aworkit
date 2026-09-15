// Real native IPC, WebView, streaming provider and separate Chat workers.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { copyFile, mkdir, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { resolve } from "node:path";
import { connectNativeWebView } from "./native-webview.mjs";

const root = resolve(`src-tauri/target/native-concurrency-${Date.now()}`);
await mkdir(root, { recursive: true });
const executable = resolve(root, "aworkit-desktop.exe");
await copyFile(resolve("src-tauri/target/debug/aworkit-desktop.exe"), executable);
const requests = [], releases = new Map();
const provider = createServer(async (request, response) => {
  if (request.method !== "POST") {
    response.setHeader("Content-Type", "application/json");
    response.end(JSON.stringify({ data: [{ id: "concurrent-fixture", context_length: 32768 }] })); return;
  }
  let raw = "";
  for await (const chunk of request) raw += chunk;
  const body = JSON.parse(raw);
  const label = body.messages.filter(m => m.role === "user").at(-1).content;
  requests.push({ label, body });
  response.setHeader("Content-Type", "text/event-stream");
  response.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { content: `Streaming ${label}. ` }, finish_reason: null }] })}\n\n`);
  await new Promise(done => setTimeout(done, 100));
  response.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { content: "Progress. " }, finish_reason: null }] })}\n\n`);
  // Keep the SSE connection live while its terminal response is held. The
  // existing synchronous provider reader observes cancellation between reads.
  const heartbeat = setInterval(() => response.write(": heartbeat\n\n"), 100);
  await new Promise(done => releases.set(label, done));
  clearInterval(heartbeat);
  response.end(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { content: `Finished ${label}.` }, finish_reason: null }] })}\n\n` +
    `data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: "stop" }], usage: { prompt_tokens: 40, completion_tokens: 20 } })}\n\n` + "data: [DONE]\n\n");
});
provider.listen(0, "127.0.0.1"); await once(provider, "listening");
const origin = `http://127.0.0.1:${provider.address().port}/v1`;
const port = 9291;
let child, view, logs = "";
const delay = () => new Promise(done => setTimeout(done, 100));
async function start() {
  child = spawn(executable, [], { windowsHide: true, stdio: ["ignore", "pipe", "pipe"], env: {
    ...process.env, AWORKIT_QA_PROFILE: root, AWORKIT_QA_HIDE_WINDOW: "1",
    WEBVIEW2_USER_DATA_FOLDER: resolve(root, "webview"), WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}`,
  } });
  child.stdout.on("data", chunk => { logs += chunk; }); child.stderr.on("data", chunk => { logs += chunk; });
  for (let n = 0; n < 200; n++) {
    try {
      view = await connectNativeWebView(`http://127.0.0.1:${port}`);
      await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke)");
      return;
    } catch { await delay(); }
  }
  throw new Error("Native startup failed: " + logs);
}
async function stop() {
  view?.close(); view = undefined;
  if (child?.exitCode === null) { const done = once(child, "exit"); child.kill(); await done; }
}
async function waitFor(expression) {
  for (let n = 0; n < 200; n++) { if (await view.evaluate(expression)) return; await delay(); }
  throw new Error(`Timed out: ${expression}\n${await view.evaluate("document.body.innerText")}\n${logs}`);
}
async function click(selector) {
  await waitFor(`Boolean(document.querySelector(${JSON.stringify(selector)} + ':not(:disabled)'))`);
  const point = await view.evaluate(`(() => {const e=document.querySelector(${JSON.stringify(selector)});e.scrollIntoView({block:'nearest'});const r=e.getBoundingClientRect();return {x:r.left+r.width/2,y:r.top+r.height/2};})()`);
  await view.command("Input.dispatchMouseEvent", { type: "mousePressed", ...point, button: "left", clickCount: 1 });
  await view.command("Input.dispatchMouseEvent", { type: "mouseReleased", ...point, button: "left", clickCount: 1 });
}
async function type(text) {
  await view.evaluate(`(() => {const e=document.querySelector('textarea[aria-label="Chat input"]');Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set.call(e,${JSON.stringify(text)});e.dispatchEvent(new Event('input',{bubbles:true}));})()`);
}
const snapshot = (id) => view.evaluate(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0,chatId:${JSON.stringify(id ?? null)}})`);
const select = id => click(`[data-chat-id="${id}"] .chat-history-link`).then(() => waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId===${JSON.stringify(id)})`));
async function send(label) {
  await type(label); await click('.composer-submit[aria-label="Send"]');
  for (let n = 0; !releases.has(label) && n < 200; n++) await delay();
  assert.ok(releases.has(label), `provider received and held ${label}`);
  return (await snapshot()).chat.chatId;
}
try {
  await start();
  await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke)");
  await view.evaluate(`(async()=>{const invoke=window.__TAURI_INTERNALS__.invoke;const s=await invoke('settings_snapshot');
    await invoke('settings_commit',{command:{commandId:'concurrency.provider',expectedVersion:s.version,appearance:'light',portableHistoryEnabled:false,provider:{baseUrl:${JSON.stringify(origin)},model:'concurrent-fixture',credentialAction:'keep',apiKey:null}}});
    const v=await invoke('settings_v2_snapshot');for(const p of v.settings.providers)for(const m of p.models){m.capabilities=['text','tools'];m.contextWindow=32768;}
    await invoke('settings_v2_commit',{command:{commandId:'concurrency.models',expectedVersion:v.version,settings:v.settings}});
    const w=await invoke('workflow_snapshot',{workflowId:'workflow.simple-chat'});w.document.nodes.find(n=>n.type==='agent').configuration.toolIds=[];
    await invoke('workflow_commit',{command:{commandId:'concurrency.workflow',expectedVersion:w.version,workflowId:'workflow.simple-chat',document:w.document}});
    await invoke('workflow_set_default',{command:{commandId:'concurrency.default',workflowId:'workflow.simple-chat'}});
  })()`);
  await view.command("Page.reload");
  await waitFor("Boolean(document.querySelector('textarea'))");
  const a = await send("Concurrent A");
  await click("button.new-chat");
  await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId!==${JSON.stringify(a)})`);
  const b = await send("Concurrent B");
  assert.equal(requests.length, 2, "both provider requests entered before either was released");
  assert.deepEqual(new Set((await snapshot()).activeChatIds), new Set([a, b]));
  await waitFor("document.querySelectorAll('.chat-busy-icon').length===2");
  await view.screenshot(resolve(root, "two-chats-running.png"));
  await click("button.new-chat");
  await waitFor("Boolean(document.querySelector('.composer-submit[aria-label=\"Send\"]'))");
  const c = (await snapshot()).chat.chatId;
  await type("Unsent draft C");
  await select(a);
  await waitFor("document.querySelector('.timeline-scroll')?.textContent.includes('Concurrent A')");
  assert.ok(!await view.evaluate("document.querySelector('.timeline-scroll')?.textContent.includes('Concurrent B')"));
  // A busy target cannot be deleted, even through direct IPC.
  assert.match(await view.evaluate(`window.__TAURI_INTERNALS__.invoke('desktop_command',{command:{schemaVersion:1,commandId:'concurrency.delete-active',expectedVersion:0,action:'delete_chat',targetId:${JSON.stringify(b)},payload:{}}}).then(()=>'',String)`), /Stop this Chat/);
  await click('button[aria-label="Stop response"]');
  await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>!s.activeChatIds.includes(${JSON.stringify(a)}))`);
  assert.ok((await snapshot()).activeChatIds.includes(b), "Stop A does not stop B");
  releases.get("Concurrent A")();
  await select(c);
  await waitFor("document.querySelector('textarea')?.value==='Unsent draft C'");
  // A concurrent Settings commit must survive the worker's completion feedback.
  await view.evaluate(`(async()=>{const invoke=window.__TAURI_INTERNALS__.invoke;const s=await invoke('settings_v2_snapshot');s.settings.appearance.mode='dark';await invoke('settings_v2_commit',{command:{commandId:'concurrency.dark',expectedVersion:s.version,settings:s.settings}});})()`);
  releases.get("Concurrent B")();
  await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.activeChatIds.length===0)");
  assert.equal((await snapshot()).chat.chatId, c, "background completion preserves selection");
  assert.equal(await view.evaluate("document.querySelector('textarea').value"), "Unsent draft C");
  assert.equal(await view.evaluate("window.__TAURI_INTERNALS__.invoke('settings_v2_snapshot').then(s=>s.settings.appearance.mode)"), "dark");
  const sa = await snapshot(a), sb = await snapshot(b);
  assert.equal(sa.chat.recoveryPending, false);
  assert.ok(sa.events.some(e => e.kind === "chat.turn_stopped"));
  assert.ok(!JSON.stringify(sa.events).includes("Concurrent B"));
  assert.ok(!JSON.stringify(sb.events).includes("Concurrent A"));
  await select(b);
  await waitFor("document.querySelector('.timeline-scroll')?.textContent.includes('Finished Concurrent B')");
  await view.screenshot(resolve(root, "completed-chat.png"));
  // Restart with one unresolved effect: recovery stays local to its Chat.
  await click("button.new-chat");
  await waitFor("Boolean(document.querySelector('.composer-submit[aria-label=\"Send\"]'))");
  const recovery = await send("Recovery C");
  await stop(); releases.get("Recovery C")();
  await start();
  await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.recoveryPending)");
  assert.equal(requests.length, 3, "restart does not replay a pending effect");
  await click("button.new-chat");
  await waitFor("Boolean(document.querySelector('.composer-submit[aria-label=\"Send\"]'))");
  const d = await send("After recovery"); releases.get("After recovery")();
  await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.activeChatIds.length===0)");
  assert.equal((await snapshot(recovery)).chat.recoveryPending, true);
  assert.equal((await snapshot()).chat.chatId, d);
  assert.equal(requests.length, 4);
  await writeFile(resolve(root, "result.json"), JSON.stringify({ passed: true, chats: { a, b, c, recovery, d }, providerCalls: requests.length, checks: ["overlap", "navigation", "busy icons", "draft retention", "Stop isolation", "Settings preservation", "event isolation", "restart recovery isolation"] }, null, 2));
  console.log(JSON.stringify({ passed: true, root, providerCalls: requests.length }));
} catch (error) {
  console.error("Native artifacts:", root, error);
  if (view) {
    try {
      await writeFile(resolve(root, "failed-ui.txt"), await view.evaluate("document.body.innerText"));
      await writeFile(resolve(root, "failed-snapshot.json"), JSON.stringify(await snapshot(), null, 2));
      await view.screenshot(resolve(root, "failed.png"));
    } catch (diagnostic) { console.error("Could not collect failure snapshot:", diagnostic); }
  }
  throw error;
} finally {
  await writeFile(resolve(root, "requests.json"), JSON.stringify(requests, null, 2));
  for (const release of releases.values()) release();
  await writeFile(resolve(root, "native.log"), logs);
  await stop(); provider.closeAllConnections(); provider.close();
}
