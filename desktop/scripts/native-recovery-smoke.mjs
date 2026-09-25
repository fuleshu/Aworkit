// Native recovery controls and stopped questions, using an isolated QA profile.
import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { mkdir, writeFile, copyFile } from "node:fs/promises";
import { resolve } from "node:path";
import { once } from "node:events";
import assert from "node:assert/strict";
import { connectNativeWebView } from "./native-webview.mjs";

const mode = process.argv[2] ?? "recovery";
assert.ok(["stop", "recovery"].includes(mode));
const root = resolve(`src-tauri/target/native-recovery-${Date.now()}`);
const project = resolve(root, "project");
await mkdir(project, { recursive: true });
const executable = resolve(root, "aworkit-desktop.exe");
await copyFile(resolve("src-tauri/target/debug/aworkit-desktop.exe"), executable);
const requests = [];
const server = createServer(async (request, response) => {
  response.setHeader("Content-Type", "application/json");
  if (request.url === "/v1/models") return response.end(JSON.stringify({ data: [{ id: "ask-user-fixture" }] }));
  const chunks = []; for await (const chunk of request) chunks.push(chunk);
  const body = JSON.parse(Buffer.concat(chunks).toString());
  requests.push(body);
  let message;
  if (body.messages.some((entry) => entry.role === "tool")) {
    // Crash the isolated app while its answer continuation is in flight.
    if (mode === "recovery") return;
    // The answer arrived: echo exactly what the tool loop delivered.
    const answer = body.messages.filter((entry) => entry.role === "tool").at(-1)?.content ?? "";
    message = { role: "assistant", content: `Recorded the answer: ${answer}` };
  } else {
    const name = body.tools?.find((tool) => tool.function.name === "ask_user")?.function.name;
    if (!name) throw new Error("Fixture expected the ask_user tool");
    message = {
      role: "assistant",
      content: null,
      tool_calls: [{
        id: "fixture.ask",
        type: "function",
        function: {
          name,
          arguments: JSON.stringify({
            prompt: "Which release channel should this build target?",
            title: "Release channel",
            options: [
              { id: "stable", label: "Stable" },
              { id: "beta", label: "Beta", description: "Early access" },
            ],
            allowFreeText: true,
          }),
        },
      }],
    };
  }
  const usage = { prompt_tokens: 17, completion_tokens: 9, total_tokens: 26 };
  if (body.stream) {
    const delta = message.tool_calls
      ? { role: "assistant", tool_calls: message.tool_calls.map((call, index) => ({ index, ...call })) }
      : { role: "assistant", content: message.content };
    for (const chunk of [
      { choices: [{ index: 0, delta, finish_reason: null }] },
      { choices: [{ index: 0, delta: {}, finish_reason: message.tool_calls ? "tool_calls" : "stop" }] },
      { choices: [], usage },
    ]) response.write(`data: ${JSON.stringify({ id: "fixture-response", object: "chat.completion.chunk", model: "ask-user-fixture", ...chunk })}\n\n`);
    response.end("data: [DONE]\n\n");
  } else {
    response.end(JSON.stringify({ id: "fixture-response", object: "chat.completion", model: "ask-user-fixture", choices: [{ index: 0, finish_reason: message.tool_calls ? "tool_calls" : "stop", message }], usage }));
  }
});
server.listen(0, "127.0.0.1");
await once(server, "listening");
const origin = `http://127.0.0.1:${server.address().port}`;

let child, view, logs = "";
let debugPort = 9247;
const delay = () => new Promise(resolve => setTimeout(resolve, 100));
const evaluate = expression => view.evaluate(expression);
const command = (method, params) => view.command(method, params);
const snapshot = () => evaluate("window.__TAURI_INTERNALS__.invoke('desktop_snapshot', {afterSequence:0})");
async function start() {
  child = spawn(executable, [], { windowsHide: true, stdio: ["ignore", "pipe", "pipe"], env: {
    ...process.env, AWORKIT_QA_PROFILE: root, AWORKIT_QA_HIDE_WINDOW: "1",
    WEBVIEW2_USER_DATA_FOLDER: resolve(root, "webview"),
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${debugPort++}`,
  } });
  child.stdout.on("data", chunk => { logs += chunk; });
  child.stderr.on("data", chunk => { logs += chunk; });
  for (let n = 0; n < 200; n++) {
    try {
      view = await connectNativeWebView(`http://127.0.0.1:${debugPort - 1}`);
      await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke)");
      return;
    } catch { await delay(); }
  }
  throw new Error("Native startup failed: " + logs);
}
async function stop() {
  view?.close();
  if (child?.exitCode === null) { const done = once(child, "exit"); child.kill(); await done; }
  server.closeAllConnections();
}
async function waitFor(expression, attempts = 200) {
  for (let n = 0; n < attempts; n++) { if (await evaluate(expression)) return; await delay(); }
  throw new Error(`Timed out: ${expression}\n${await evaluate("document.body.innerText")}\n${logs}`);
}
async function click(name) {
  const find = `[...document.querySelectorAll('button')].find(button => (button.getAttribute('aria-label') === ${JSON.stringify(name)} || button.title === ${JSON.stringify(name)} || button.textContent.trim() === ${JSON.stringify(name)}) && !button.disabled)`;
  await waitFor(`Boolean(${find})`);
  const point = await evaluate(`(() => { const e = ${find}; e.scrollIntoView({block:'nearest'}); const r = e.getBoundingClientRect(); return {x:r.left+r.width/2,y:r.top+r.height/2}; })()`);
  await command("Input.dispatchMouseEvent", { type: "mousePressed", ...point, button: "left", clickCount: 1 });
  await command("Input.dispatchMouseEvent", { type: "mouseReleased", ...point, button: "left", clickCount: 1 });
}
async function select(label, value) {
  await waitFor(`Boolean(document.querySelector('select[aria-label="${label}"]:not(:disabled)'))`);
  await evaluate(`(() => { const e=document.querySelector('select[aria-label="${label}"]');Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype,'value').set.call(e,${JSON.stringify(value)});e.dispatchEvent(new Event('change',{bubbles:true})); })()`);
}
async function type(label, value) {
  await waitFor(`Boolean(document.querySelector('textarea[aria-label="${label}"]:not(:disabled)'))`);
  await evaluate(`(() => { const e=document.querySelector('textarea[aria-label="${label}"]');Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set.call(e,${JSON.stringify(value)});e.dispatchEvent(new Event('input',{bubbles:true})); })()`);
}
try {
  await start();
  await evaluate(`(async () => {
    const invoke = window.__TAURI_INTERNALS__.invoke;
    const settings = await invoke('settings_snapshot');
    await invoke('settings_commit', { command: { commandId:'native.ask.configure', expectedVersion:settings.version, appearance:'system', portableHistoryEnabled:false, provider:{baseUrl:${JSON.stringify(origin + "/v1")}, model:'ask-user-fixture', credentialAction:'keep', apiKey:null} } });
    const v2 = await invoke('settings_v2_snapshot');
    for (const provider of v2.settings.providers) for (const model of provider.models) model.capabilities = ['text','tools'];
    for (const tool of v2.settings.tools) tool.enabled = true;
    v2.settings.projects = [{id:'project.ask',name:'Ask fixture',workspace:{kind:'local_directory',location:${JSON.stringify(project)}},defaultWorkflowId:'workflow.simple-chat',portableHistoryEnabled:false}];
    await invoke('settings_v2_commit', {command:{commandId:'native.ask.enable',expectedVersion:v2.version,settings:v2.settings}});
    const workflow = await invoke('workflow_snapshot',{workflowId:'workflow.simple-chat'});
    const agent = workflow.document.nodes.find(node => node.type === 'agent');
    agent.configuration.toolIds = ['tool.ask_user'];
    await invoke('workflow_commit',{command:{commandId:'native.ask.workflow',expectedVersion:workflow.version,workflowId:'workflow.simple-chat',document:workflow.document}});
  })()`);
  await command("Page.reload");
  await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke)");
  const previous = (await snapshot()).chat.chatId;
  await click("Create a new Chat");
  await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(value => value.chat.chatId !== ${JSON.stringify(previous)} && value.chat.phase === 'draft')`);
  await select("Workflow for the first Chat input", "workflow.simple-chat");
  await select("Project for the first Chat input", "project.ask");
  await type("Chat input", "Ask me which channel to target.");
  await click("Send");
  // The question must raise its dialog, and the option plus Submit must work.
  await waitFor("Boolean(document.querySelector('dialog[open]'))", 300);
  const dialogText = await evaluate("document.querySelector('dialog[open]')?.textContent ?? ''");
  assert.match(dialogText, /Which release channel should this build target\?/);

  if (mode === "stop") {
    await click("Decide later");
    await click("Stop response");
    await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s => s.chat.phase === 'waiting_input')");
    await command("Page.reload");
    await waitFor("Boolean(document.querySelector('textarea[aria-label=\"Chat input\"]:not(:disabled)'))");
    assert.equal(await evaluate("Boolean(document.querySelector('dialog[open]'))"), false);
    const settled = await snapshot();
    assert.ok(settled.events.some(e => e.kind === "question.cancelled"));
    assert.equal(settled.chat.recoveryPending, false);
    assert.equal(requests.length, 1, "Stop never resumes the model");
  } else {
    await type("Your own answer", "Use preview");
    await click("Submit answer");
    for (let n=0; requests.length < 2 && n < 200; n++) await delay();
    assert.equal(requests.length, 2);
    await stop();
    await start();
    await waitFor("Boolean(document.querySelector('.chat-recovery-card'))");
    if (await evaluate("document.querySelector('button[title=\"Show or hide Run details\"]')?.getAttribute('aria-pressed') === 'true'")) await click("Run details");
    const chatId = (await snapshot()).chat.chatId;
    // Recovery stays in the conversation layout, with no focus trap or modal backdrop.
    const layout = await evaluate(`(() => {
      const card=document.querySelector('.chat-recovery-card'), composer=document.querySelector('.composer-shell');
      const c=card.getBoundingClientRect(), m=composer.getBoundingClientRect();
      return {above:c.bottom <= m.top, width:c.width, height:c.height, modal:!!document.querySelector('dialog[open]'), position:getComputedStyle(card).position};
    })()`);
    assert.ok(layout.above && layout.height < 180);
    assert.equal(layout.modal, false);
    assert.equal(layout.position, "static");
    await view.screenshot(resolve(root, "card.png"));
    await command("Emulation.setDeviceMetricsOverride", {width:820,height:900,deviceScaleFactor:1,mobile:false});
    await waitFor("document.querySelector('.chat-recovery-card').getBoundingClientRect().width < 800");
    assert.ok(await evaluate(`[...document.querySelectorAll('.chat-recovery-actions button')].every(e => { const r=e.getBoundingClientRect(); return e.scrollWidth <= e.clientWidth && r.right <= innerWidth && r.left >= 0; })`));
    await view.screenshot(resolve(root, "card-narrow.png"));
    await command("Emulation.clearDeviceMetricsOverride");
    await click("Continue reply");
    await waitFor("Boolean(document.querySelector('.chat-recovery-error'))");
    assert.ok((await snapshot()).chat.recoveryPending, "refused recovery stays actionable");
    assert.equal(requests.length, 2, "ambiguous provider work is never replayed");
    await view.screenshot(resolve(root, "recovery-error.png"));
    await click("Stop reply");
    assert.equal(await evaluate("Boolean(document.querySelector('dialog[open]'))"), false);
    await click("Go back");
    assert.ok((await snapshot()).chat.recoveryPending, "back does not stop");
    await click("Stop reply");
    await click("Stop reply");
    await waitFor("!document.querySelector('.chat-recovery-card')");
    const settled = await snapshot();
    assert.equal(settled.chat.chatId, chatId);
    assert.equal(settled.chat.recoveryPending, false);
    assert.equal(settled.chat.phase, "waiting_input");
    assert.equal(requests.length, 2);
    await waitFor("Boolean(document.querySelector('textarea[aria-label=\"Chat input\"]:not(:disabled)'))");
    assert.equal(await evaluate("Boolean(document.querySelector('.run-failure-banner'))"), false);
  }
  await view.screenshot(resolve(root, "result.png"));
  await writeFile(resolve(root, "result.json"), JSON.stringify({ok:true, mode, root, requests:requests.length, snapshot:await snapshot()},null,2));
  console.log(JSON.stringify({ok:true,mode,root,requests:requests.length}));
} finally {
  await writeFile(resolve(root, "app.log"), logs);
  await stop(); server.close();
}
