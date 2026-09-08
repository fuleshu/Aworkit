// Native plugin discovery, typed settings, workflow binding and real MCP execution.
// Uses only an isolated profile, a local deterministic provider and the MCP fixture.
import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { once } from "node:events";
import assert from "node:assert/strict";
import { connectNativeWebView } from "./native-webview.mjs";

const root = resolve("src-tauri/target/native-tool-plugins-" + Date.now());
const folder = resolve(root, "runtime/tool-plugins/echo");
await mkdir(folder, { recursive: true });
const python = process.env.AWORKIT_QA_PYTHON ?? "C:\\Python313\\python.exe";
const manifestPath = resolve(folder, "tool-plugin.json");
await writeFile(manifestPath, JSON.stringify({
  schemaVersion: 1, id: "plugin.echo", name: "Fixture MCP", version: "1.0.0",
  execution: { transport: "stdio", command: python,
    args: [resolve("scripts/fixtures/mcp-server-selection.py")], env: [] },
}, null, 2));

const requests = [];
let approvalBatch = false;
const provider = createServer(async (request, response) => {
  if (request.url === "/v1/models") return response.end(JSON.stringify({ data: [{ id: "tool-fixture" }] }));
  const chunks = []; for await (const chunk of request) chunks.push(chunk);
  const body = JSON.parse(Buffer.concat(chunks).toString()); requests.push(body);
  const toolResult = body.messages.find(message => message.role === "tool");
  const name = body.tools?.find(tool => tool.function.name.endsWith("__echo"))?.function.name;
  const message = !name ? { role: "assistant", content: JSON.stringify({ goal: "Echo requested text", openQuestions: [], evidenceNeeded: [], toolOrder: ["Call echo"] }) }
    : toolResult ? { role: "assistant", content: "Native MCP plugin returned the requested echo." }
    : { role: "assistant", content: approvalBatch ? "Fixture MCP is available in my tool list." : null, tool_calls: [{ id: "fixture.echo." + requests.length, type: "function",
      function: { name, arguments: JSON.stringify({ message: "native-plugin-proof" }) } }] };
  if (approvalBatch && message.tool_calls) message.tool_calls.unshift({ id: "fixture.describe." + requests.length, type: "function",
    function: { name: body.tools.find(t=>t.function.name.endsWith("__describe")).function.name,
      arguments: JSON.stringify({message:"native-plugin-context"}) } });
  const usage = { prompt_tokens: 20, completion_tokens: 10, total_tokens: 30 };
  if (body.stream) {
    response.setHeader("Content-Type", "text/event-stream");
    const delta = message.tool_calls ? { role: "assistant", content: message.content, tool_calls: message.tool_calls.map((call, index) => ({ index, ...call })) } : message;
    for (const chunk of [
      { choices: [{ index: 0, delta, finish_reason: null }] },
      { choices: [{ index: 0, delta: {}, finish_reason: message.tool_calls ? "tool_calls" : "stop" }] },
      { choices: [], usage },
    ]) response.write("data: " + JSON.stringify({ id: "fixture-response", object: "chat.completion.chunk", model: "tool-fixture", ...chunk }) + "\n\n");
    response.end("data: [DONE]\n\n");
  } else response.end(JSON.stringify({ id: "fixture-response", object: "chat.completion", model: "tool-fixture",
    choices: [{ index: 0, message, finish_reason: message.tool_calls ? "tool_calls" : "stop" }], usage }));
});
provider.listen(0, "127.0.0.1"); await once(provider, "listening");
const origin = "http://127.0.0.1:" + provider.address().port;
const port = 9252;
const launch = () => spawn(resolve(process.env.AWORKIT_QA_EXE ?? "src-tauri/target/debug/aworkit-desktop.exe"), [], {
  windowsHide: true, stdio: "ignore", env: { ...process.env, AWORKIT_QA_PROFILE: root,
    WEBVIEW2_USER_DATA_FOLDER: resolve(root, "webview"),
    AWORKIT_QA_HIDE_WINDOW: "1", WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: "--remote-debugging-port=" + port },
});
let child = launch(), view;
async function connect() {
  for (let i = 0; i < 100; i++) {
    try { return await connectNativeWebView("http://127.0.0.1:" + port, process.env.AWORKIT_QA_PAGE_URL ?? "http://tauri.localhost"); } catch {}
    await new Promise(resolve => setTimeout(resolve, 150));
  }
  throw new Error("Native WebView did not start");
}
const evaluate = expression => view.evaluate(expression);
async function waitFor(expression) {
  for (let i = 0; i < 150; i++) {
    if (await evaluate(expression)) return;
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  throw new Error("Timed out: " + expression + "\n" + await evaluate("document.body.innerText"));
}
const click = name => waitFor("(() => { const b=[...document.querySelectorAll('button')].find(b=>!b.disabled&&(b.title===" + JSON.stringify(name) + "||b.getAttribute('aria-label')===" + JSON.stringify(name) + "||b.textContent.trim()===" + JSON.stringify(name) + "||b.textContent.trim().replace(/^[＋⚙◇]\\s*/,'').startsWith(" + JSON.stringify(name) + "))); if(!b)return false;b.click();return true;})()");
async function setValue(selector, value) {
  await waitFor("Boolean(document.querySelector(" + JSON.stringify(selector) + "))");
  await evaluate("(() => {const e=document.querySelector(" + JSON.stringify(selector) + ");const p=e.tagName==='SELECT'?HTMLSelectElement.prototype:e.tagName==='TEXTAREA'?HTMLTextAreaElement.prototype:HTMLInputElement.prototype;Object.getOwnPropertyDescriptor(p,'value').set.call(e," + JSON.stringify(value) + ");e.dispatchEvent(new Event(e.tagName==='SELECT'?'change':'input',{bubbles:true}));})()");
}
const settingsSnapshot = () => evaluate("window.__TAURI_INTERNALS__.invoke('settings_v2_snapshot')");
try {
  view = await connect();
  await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke)");
  await evaluate("(async () => {const invoke=window.__TAURI_INTERNALS__.invoke;const s=await invoke('settings_snapshot');await invoke('settings_commit',{command:{commandId:'plugin.qa.provider',expectedVersion:s.version,appearance:'system',portableHistoryEnabled:false,provider:{baseUrl:" + JSON.stringify(origin + "/v1") + ",model:'tool-fixture',credentialAction:'keep',apiKey:null}}});const v=await invoke('settings_v2_snapshot');for(const p of v.settings.providers)for(const m of p.models)m.capabilities=['text','tools'];v.settings.tools.find(t=>t.id==='tool.web_fetch').enabled=true;await invoke('settings_v2_commit',{command:{commandId:'plugin.qa.enable',expectedVersion:v.version,settings:v.settings}});})()");
  await view.command("Page.reload");
  await click("Settings");
  await click("Tools");
  await waitFor("document.body.innerText.includes('Fixture MCP')");
  const initial = await settingsSnapshot();
  assert.equal(initial.toolPlugins[0].server.enabled, false);
  assert.equal(initial.settings.mcpServers.length, 0, "Discovery did not enable or persist the plugin");
  await setValue("#tool\\.web_fetch-instructions", "Native fetch guidance kept outside the echo-only workflow.");
  await setValue("#tool\\.shell\\.host-executable", "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe");
  await click("Add plugin");
  await click("MCP servers");
  await waitFor("document.body.innerText.includes('Fixture MCP')");
  assert.ok(await evaluate("document.body.innerText.includes('Setup required')"));
  await evaluate("[...document.querySelectorAll('label')].find(l=>l.textContent.trim()==='Enable MCP server').querySelector('input').click()");
  await waitFor("Boolean(document.querySelector('details summary')) && [...document.querySelectorAll('details summary')].some(s=>s.textContent==='echo')");
  await evaluate("[...document.querySelectorAll('details summary')].find(s=>s.textContent==='echo').click()");
  await setValue("#plugin\\.echo-echo-instructions", "Use echo to return the requested text. Echo only.");
  await setValue("#plugin\\.echo-echo-approval-mode", "full_access");
  await evaluate("document.getElementById('plugin.echo-echo-instructions').scrollIntoView({block:'center'})");
  await view.screenshot(resolve(root, "mcp-settings.png"));
  await click("Save configuration");
  await waitFor("window.__TAURI_INTERNALS__.invoke('settings_v2_snapshot').then(s=>s.settings.mcpServers[0]?.tools?.find(t=>t.name==='echo')?.options?.approvalMode==='full_access')");
  const saved = await settingsSnapshot();
  assert.equal(saved.settings.tools.find(t=>t.id==="tool.web_fetch").options.instructions, "Native fetch guidance kept outside the echo-only workflow.");
  assert.equal(saved.settings.mcpServers[0].enabled, true);
  assert.equal(saved.settings.mcpServers[0].tools.length, 2);
  assert.equal(saved.settings.mcpServers[0].tools.find(t=>t.name==='echo').inputSchema.properties.message.type, "string");
  await click("Back to Chat");
  // Select the whole server in the actual editor; both functions must reach the provider.
  await evaluate("(async()=>{const invoke=window.__TAURI_INTERNALS__.invoke;const w=await invoke('workflow_snapshot',{workflowId:'workflow.standard-agent'});w.document.nodes.find(n=>n.type==='agent').configuration.toolIds=[];await invoke('workflow_commit',{command:{commandId:'plugin.qa.workflow',expectedVersion:w.version,workflowId:'workflow.standard-agent',document:w.document}})})()");
  await click("Workflows");
  await waitFor("Boolean(document.querySelector('select'))");
  await evaluate("(() => { const s=[...document.querySelectorAll('select')].find(s=>[...s.options].some(o=>o.value==='workflow.standard-agent')); if(!s)throw new Error('Workflow selector unavailable');Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype,'value').set.call(s,'workflow.standard-agent');s.dispatchEvent(new Event('change',{bubbles:true})); })()");
  await waitFor("document.querySelector('.workflow-library-bar select')?.value === 'workflow.standard-agent' && document.body.innerText.includes('Version 2')");
  await evaluate("[...document.querySelectorAll('[aria-label=\"Workflow nodes\"] button')].find(b=>b.textContent==='Agent').click()");
  await waitFor("Boolean(document.querySelector('input[title=\"Use all enabled Fixture MCP functions in this Agent\"]'))");
  assert.equal(await evaluate("document.querySelectorAll('.tool-multi-field input[type=checkbox][title*=\"Fixture MCP\"]').length"), 1);
  await evaluate("document.querySelector('input[title=\"Use all enabled Fixture MCP functions in this Agent\"]').click()");
  await view.screenshot(resolve(root, "workflow-tools.png"));
  await click("Save");
  await waitFor("window.__TAURI_INTERNALS__.invoke('workflow_snapshot',{workflowId:'workflow.standard-agent'}).then(w=>w.document.nodes.find(n=>n.type==='agent').configuration.toolIds.includes('mcp:plugin.echo'))");
  const previousChat = await evaluate("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId)");
  await click("New Chat");
  await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId!==" + JSON.stringify(previousChat) + " && s.chat.phase==='draft')");
  await setValue('select[aria-label="Workflow for the first Chat input"]', "workflow.standard-agent");
  await setValue('select[aria-label="Approval mode"]', "ask_for_approval");
  await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.approvalMode==='ask_for_approval')");
  await setValue('textarea[aria-label="Chat input"]', "Echo native-plugin-proof.");
  await click("Send");
  await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.phase==='waiting_input')");
  const agentRequests = requests.filter(request => request.tools?.length);
  assert.equal(agentRequests.length, 2, "Exactly one tool call and its continuation");
  const first = agentRequests[0], prompt = first.messages.filter(m=>m.role==="system").map(m=>m.content).join("\n");
  assert.ok(prompt.includes("You are Aworkit, a helpful"));
  assert.ok(prompt.includes("Use echo to return the requested text. Echo only."));
  assert.ok(!prompt.includes("Native fetch guidance") && !prompt.includes("Tool instructions for tool.web_search:"));
  assert.equal(first.tools.length, 2);
  assert.ok(first.tools.some(tool => tool.function.name.includes('describe')));
  assert.ok(agentRequests[1].messages.some(m=>m.role==="tool" && m.content.includes("native-plugin-proof")));
  await view.screenshot(resolve(root, "chat.png"));
  await evaluate("(async()=>{const invoke=window.__TAURI_INTERNALS__.invoke;const s=await invoke('settings_v2_snapshot');s.settings.mcpServers[0].transport.args.unshift('-u');s.settings.mcpServers[0].tools.find(t=>t.name==='echo').options.instructions='Updated echo guidance for new Chats.';await invoke('settings_v2_commit',{command:{commandId:'plugin.qa.update',expectedVersion:s.version,settings:s.settings}});})()");
  const updatedSettings = await settingsSnapshot();
  const firstChat = await evaluate("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId)");
  await click("New Chat");
  await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId!==" + JSON.stringify(firstChat) + " && s.chat.phase==='draft')");
  await setValue('select[aria-label="Workflow for the first Chat input"]', "workflow.standard-agent");
  await setValue('textarea[aria-label="Chat input"]', "Echo native-plugin-proof with the updated plugin.");
  await click("Send");
  await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.phase==='waiting_input')");
  assert.equal(requests.length, 6, "The updated connection starts in a new Chat without restarting the app");
  assert.ok(requests[4].messages.some(m=>m.role==='system' && m.content.includes('Updated echo guidance for new Chats.')));
  // Current settings intentionally stop working. The existing Chat must retain
  // its earlier instructions and exact executable arguments across restart.
  await evaluate("(async()=>{const invoke=window.__TAURI_INTERNALS__.invoke;const s=await invoke('settings_v2_snapshot');s.settings.mcpServers[0].transport.args=['--invalid-new-settings'];s.settings.mcpServers[0].tools.find(t=>t.name==='echo').options.instructions='Future settings must not leak into this Chat.';await invoke('settings_v2_commit',{command:{commandId:'plugin.qa.future',expectedVersion:s.version,settings:s.settings}});})()");
  const futureSettings = await settingsSnapshot();
  // Reopen the same isolated profile: catalog, overrides and enablement must persist.
  view.close(); view = undefined;
  const exited = once(child, "exit"); child.kill(); await exited;
  child = launch(); view = await connect();
  await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke)");
  const reopened = await settingsSnapshot();
  assert.deepEqual(reopened.settings.mcpServers, futureSettings.settings.mcpServers);
  const beforeFollowUp = await evaluate("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.expectedVersion)");
  await setValue('textarea[aria-label="Chat input"]', "Echo native-plugin-proof after restart.");
  await click("Queue");
  await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.phase==='waiting_input' && s.chat.expectedVersion>" + beforeFollowUp + ")");
  assert.equal(requests.length, 9, "The existing MCP Chat reconnects after restart");
  assert.ok(requests[7].messages.some(m=>m.role==='system' && m.content.includes('Updated echo guidance for new Chats.') && !m.content.includes('Future settings')));
  // A saved approval must reconnect the same plugin before resuming after restart.
  await evaluate("(async()=>{const invoke=window.__TAURI_INTERNALS__.invoke;const s=await invoke('settings_v2_snapshot');s.settings.mcpServers=" + JSON.stringify(updatedSettings.settings.mcpServers) + ";s.settings.mcpServers[0].tools.find(t=>t.name==='echo').options.approvalMode='ask_for_approval';await invoke('settings_v2_commit',{command:{commandId:'plugin.qa.approval',expectedVersion:s.version,settings:s.settings}});})()");
  await click("New Chat");
  await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.phase==='draft')");
  await setValue('select[aria-label="Workflow for the first Chat input"]', "workflow.standard-agent");
  await setValue('textarea[aria-label="Chat input"]', "Echo native-plugin-proof after approval.");
  await click("Send");
  await waitFor("[...document.querySelectorAll('button')].some(b=>b.textContent==='Approve once'&&!b.disabled)");
  assert.equal(requests.length, 11);
  view.close(); view = undefined;
  const approvalExit = once(child, "exit"); child.kill(); await approvalExit;
  child = launch(); view = await connect();
  await click("Approve once");
  await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.phase==='waiting_input')");
  assert.equal(requests.length, 12, "Approval resume executes the pending MCP call without replaying the model");
  assert.ok(requests[11].messages.some(m=>m.role==='tool' && m.content.includes('native-plugin-proof')));
  // A completed call and the model's own availability statement precede the
  // approval in one response. Denial after restart must retain both of them.
  approvalBatch = true;
  await evaluate("(async()=>{const invoke=window.__TAURI_INTERNALS__.invoke;const s=await invoke('settings_v2_snapshot');s.settings.mcpServers[0].tools.find(t=>t.name==='describe').options={approvalMode:'full_access'};await invoke('settings_v2_commit',{command:{commandId:'plugin.qa.denial-context',expectedVersion:s.version,settings:s.settings}});})()");
  await click("New Chat");
  await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.phase==='draft')");
  await setValue('select[aria-label="Workflow for the first Chat input"]', "workflow.standard-agent");
  await setValue('textarea[aria-label="Chat input"]', "Can you see the Fixture MCP tools?");
  await click("Send");
  await waitFor("[...document.querySelectorAll('button')].some(b=>b.textContent==='Approve once'&&!b.disabled)");
  assert.equal(requests.length,14);
  view.close(); view = undefined;
  const denialExit = once(child, "exit"); child.kill(); await denialExit;
  child = launch(); view = await connect();
  await click("Deny and give reason");
  await setValue('textarea[aria-label="Reason for denial"]', "Just answer whether the tools are available.");
  await click("Deny action");
  await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.phase==='waiting_input')");
  assert.equal(requests.length,15,"Denial resumes the same response without repeating the model or completed call");
  assert.deepEqual(requests[14].tools,requests[13].tools,"MCP definitions remain identical after denial");
  const continued = requests[14].messages.find(m=>m.tool_calls?.length);
  assert.equal(continued.content,"Fixture MCP is available in my tool list.");
  assert.equal(continued.tool_calls.length,2);
  const results = requests[14].messages.filter(m=>m.role==='tool');
  assert.equal(results.length,2);
  assert.ok(results[0].content.includes('native-plugin-context'));
  assert.ok(results[1].content.includes('user_rejected') && results[1].content.includes('Just answer whether the tools are available.'));
  await view.screenshot(resolve(root,"denial-context.png"));
  const report = { ok: true, root, providerCalls: requests.length, cases: [
    "inert folder discovery", "typed instructions and executable settings", "live MCP discovery",
    "shared workflow selector", "per-tool approval override", "selected instructions only",
    "real MCP invocation", "new Chat uses updated connection", "restart persistence and reconnection",
    "frozen connection and instructions survive Settings edits", "MCP approval resume after restart",
    "MCP denial preserves assistant text, completed result and tool definitions after restart",
  ] };
  await writeFile(resolve(root, "report.json"), JSON.stringify(report, null, 2));
  await writeFile(resolve(root, "provider-requests.json"), JSON.stringify(requests, null, 2));
  console.log(JSON.stringify(report));
} catch (error) {
  await writeFile(resolve(root, "failure.txt"), String(error));
  if(view) { await view.screenshot(resolve(root, "failure.png")).catch(()=>{}); }
  throw error;
} finally { view?.close(); child.kill(); provider.close(); }
