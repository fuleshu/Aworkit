// Exercises the built Windows WebView against an isolated profile and a local
// provider fixture. Approval choices use real UI controls and native IPC.
import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { once } from "node:events";
import assert from "node:assert/strict";

const root = resolve(`src-tauri/target/native-approvals-${Date.now()}`);
const project = resolve(root, "project");
await mkdir(project, { recursive: true });
const file = resolve(project, "approval.txt");
await writeFile(file, "original");
const requests = [];
let reviewerDecision = "approve";
const server = createServer(async (request, response) => {
  response.setHeader("Content-Type", "application/json");
  if (request.url === "/v1/models") return response.end(JSON.stringify({ data: [{ id: "approval-fixture" }] }));
  const chunks = []; for await (const chunk of request) chunks.push(chunk);
  const body = JSON.parse(Buffer.concat(chunks).toString());
  requests.push(body);
  const isReview = body.messages.some(message => typeof message.content === "string" && message.content.includes("independent approval reviewer"));
  let message;
  if (isReview) message = { role: "assistant", content: JSON.stringify({ decision: reviewerDecision, reason: reviewerDecision === "approve" ? "The user requested this project edit." : "Preserve the original file." }) };
  else if (body.messages.some(message => message.role === "tool")) message = { role: "assistant", content: "Action settled." };
  else {
    const name = body.tools?.find(tool => tool.function.name.includes("write"))?.function.name;
    if (!name) throw new Error("Fixture expected the project write tool");
    message = { role: "assistant", content: null, tool_calls: [{ id: "fixture.write", type: "function", function: { name, arguments: JSON.stringify({ path: "approval.txt", content: "updated" }) } }] };
  }
  const usage = { prompt_tokens: 17, completion_tokens: 9, total_tokens: 26 };
  if (body.stream) {
    response.setHeader("Content-Type", "text/event-stream");
    const delta = message.tool_calls ? { role: "assistant", tool_calls: message.tool_calls.map((call, index) => ({ index, ...call })) } : message;
    for (const chunk of [
      { choices: [{ index: 0, delta, finish_reason: null }] },
      { choices: [{ index: 0, delta: {}, finish_reason: message.tool_calls ? "tool_calls" : "stop" }] },
      { choices: [], usage },
    ]) response.write(`data: ${JSON.stringify({ id: "fixture-response", object: "chat.completion.chunk", model: "approval-fixture", ...chunk })}\n\n`);
    response.end("data: [DONE]\n\n");
  } else response.end(JSON.stringify({ id: "fixture-response", object: "chat.completion", model: "approval-fixture", choices: [{ index: 0, finish_reason: message.tool_calls ? "tool_calls" : "stop", message }], usage }));
});
server.listen(0, "127.0.0.1"); await once(server, "listening");
const origin = `http://127.0.0.1:${server.address().port}`;
const port = 9237;
const child = spawn(resolve("src-tauri/target/debug/aworkit-desktop.exe"), [], { windowsHide: true, stdio: "ignore", env: { ...process.env, AWORKIT_QA_PROFILE: root, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}` } });
let socket;
try {
  let target;
  for (let attempt = 0; attempt < 100; attempt++) {
    try { target = (await fetch(`http://127.0.0.1:${port}/json/list`).then(response => response.json())).find(target => target.url.startsWith("http://tauri.localhost")); } catch {}
    if (target) break; await new Promise(resolve => setTimeout(resolve, 150));
  }
  assert.ok(target, "Native WebView is available");
  socket = new WebSocket(target.webSocketDebuggerUrl); await once(socket, "open");
  let id = 0; const pending = new Map();
  socket.addEventListener("message", ({ data }) => { const response = JSON.parse(data); const call = pending.get(response.id); if (!call) return; pending.delete(response.id); response.error ? call.reject(response.error) : call.resolve(response.result); });
  const command = (method, params = {}) => new Promise((resolve, reject) => { const next = ++id; pending.set(next, { resolve, reject }); socket.send(JSON.stringify({ id: next, method, params })); });
  const evaluate = async expression => { const result = await command("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true }); if (result.exceptionDetails) throw new Error(JSON.stringify(result.exceptionDetails)); return result.result.value; };
  const waitFor = async expression => { for (let attempt = 0; attempt < 150; attempt++) { if (await evaluate(expression)) return; await new Promise(resolve => setTimeout(resolve, 100)); } throw new Error(`Timed out: ${expression}\n${await evaluate("document.body.innerText")}`); };
  const click = async name => {
    const expression = `(() => { const button = [...document.querySelectorAll('button')].find(button => (button.getAttribute('aria-label') === ${JSON.stringify(name)} || button.title === ${JSON.stringify(name)} || button.textContent.trim().startsWith(${JSON.stringify(name)}) || button.textContent.trim().endsWith('\\n' + ${JSON.stringify(name)}) || button.textContent.trim().replace(/^[＋⚙]\\s*/, '') === ${JSON.stringify(name)}) && !button.disabled); if (!button) return false; button.click(); return true; })()`;
    await waitFor(expression);
  };
  const select = async (label, value) => { await waitFor(`Boolean(document.querySelector('select[aria-label="${label}"]:not(:disabled)'))`); return evaluate(`(() => { const input = document.querySelector('select[aria-label="${label}"]'); Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(input, ${JSON.stringify(value)}); input.dispatchEvent(new Event('change', { bubbles: true })); })()`); };
  const type = async (label, value) => evaluate(`(() => { const input = document.querySelector('textarea[aria-label="${label}"]'); if (!input || input.disabled) throw new Error('Missing enabled ${label}'); Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set.call(input, ${JSON.stringify(value)}); input.dispatchEvent(new Event('input', { bubbles: true })); })()`);
  const snapshot = () => evaluate("window.__TAURI_INTERNALS__.invoke('desktop_snapshot', {afterSequence:0})");
  await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke)");
  await evaluate(`(async () => {
    const invoke = window.__TAURI_INTERNALS__.invoke;
    const settings = await invoke('settings_snapshot');
    await invoke('settings_commit', { command: { commandId:'native.approval.configure', expectedVersion:settings.version, appearance:'system', portableHistoryEnabled:false, provider:{baseUrl:${JSON.stringify(origin + "/v1")}, model:'approval-fixture', credentialAction:'keep', apiKey:null} } });
    const v2 = await invoke('settings_v2_snapshot');
    for (const provider of v2.settings.providers) for (const model of provider.models) model.capabilities = ['text','tools'];
    for (const tool of v2.settings.tools) tool.enabled = true;
    v2.settings.projects = [{id:'project.approval',name:'Approval fixture',workspace:{kind:'local_directory',location:${JSON.stringify(project)}},defaultWorkflowId:'workflow.simple-chat',portableHistoryEnabled:false}];
    await invoke('settings_v2_commit', {command:{commandId:'native.approval.enable',expectedVersion:v2.version,settings:v2.settings}});
    const workflow = await invoke('workflow_snapshot',{workflowId:'workflow.simple-chat'});
    const agent = workflow.document.nodes.find(node => node.type === 'agent');
    agent.configuration.toolIds = ['tool.files.write'];
    await invoke('workflow_commit',{command:{commandId:'native.approval.workflow',expectedVersion:workflow.version,workflowId:'workflow.simple-chat',document:workflow.document}});
  })()`);
  await command("Page.reload");
  const start = async mode => {
    const previous = (await snapshot()).chat.chatId;
    await click("New Chat");
    await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(value => value.chat.chatId !== ${JSON.stringify(previous)} && value.chat.phase === 'draft')`);
    await waitFor("Boolean(document.querySelector('select[aria-label=\"Workflow for the first Chat input\"]:not(:disabled)'))");
    await waitFor("Boolean(document.querySelector('select[aria-label=\"Approval mode\"]:not(:disabled)'))");
    await select("Approval mode", mode);
    await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(value => value.chat.approvalMode === ${JSON.stringify(mode)})`);
    await select("Workflow for the first Chat input", "workflow.simple-chat");
    await select("Project for the first Chat input", "project.approval");
    await type("Chat input", "Write updated to approval.txt in this project.");
    await click("Send");
  };
  const waiting = () => waitFor("[...document.querySelectorAll('button')].some(button => button.textContent === 'Approve once' && !button.disabled)");
  const settled = () => waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(value => value.chat.phase === 'waiting_input')");
  await start("ask_for_approval"); await waiting();
  assert.equal(await readFile(file, "utf8"), "original");
  await command("Page.enable");
  await evaluate("[...document.querySelectorAll('button')].find(button => button.textContent === 'Approve once').closest('article').scrollIntoView({block:'center'})");
  await new Promise(resolve => setTimeout(resolve, 150));
  await writeFile(resolve(root, "approval-card.png"), Buffer.from((await command("Page.captureScreenshot", { format: "png" })).data, "base64"));
  await click("Approve once"); await settled(); assert.equal(await readFile(file, "utf8"), "updated");
  assert.equal((await evaluate("window.__TAURI_INTERNALS__.invoke('approval_project_grants')")).length, 0);
  await writeFile(file, "original");
  await start("ask_for_approval"); await waiting(); await click("Deny and give reason");
  await type("Reason for denial", "Preserve the original file."); await click("Deny action"); await settled();
  assert.equal(await readFile(file, "utf8"), "original");
  assert.ok(requests.some(request => request.messages.some(message => message.role === "tool" && message.content.includes("Preserve the original file."))));
  await start("ask_for_approval"); await waiting(); await click("Always approve in project"); await settled();
  assert.equal((await evaluate("window.__TAURI_INTERNALS__.invoke('approval_project_grants')")).length, 1);
  await writeFile(file, "original"); await start("ask_for_approval"); await settled(); assert.equal(await readFile(file, "utf8"), "updated");
  await click("Settings"); await click("Approvals"); await waitFor("document.body.innerText.includes('Saved project approvals')");
  await writeFile(resolve(root, "approval-settings.png"), Buffer.from((await command("Page.captureScreenshot", { format: "png" })).data, "base64"));
  await click("Revoke approval"); await waitFor("document.body.innerText.includes('No saved project approvals.')");
  await click("Back to Chat");
  await writeFile(file, "original"); await start("approve_for_me"); await settled(); assert.equal(await readFile(file, "utf8"), "updated");
  const reviewed = (await snapshot()).events.filter(event => event.kind === "approval.reviewed"); assert.equal(reviewed.length, 1); assert.equal(reviewed[0].payload.decision, "approve");
  reviewerDecision = "deny"; await writeFile(file, "original"); await start("approve_for_me"); await settled(); assert.equal(await readFile(file, "utf8"), "original");
  const reviewCount = requests.filter(request => request.messages.some(message => typeof message.content === "string" && message.content.includes("independent approval reviewer"))).length;
  await start("full_access"); await settled(); assert.equal(await readFile(file, "utf8"), "updated");
  assert.equal(requests.filter(request => request.messages.some(message => typeof message.content === "string" && message.content.includes("independent approval reviewer"))).length, reviewCount);
  await writeFile(resolve(root, "result.json"), JSON.stringify({ ok: true, root, cases: ["approve once", "deny with reason", "always in project", "reuse project grant", "revoke grant", "automatic approval", "automatic denial", "full access"] }, null, 2));
  console.log(JSON.stringify({ ok: true, root, requests: requests.length }));
} finally {
  socket?.close(); child.kill(); server.close();
}
