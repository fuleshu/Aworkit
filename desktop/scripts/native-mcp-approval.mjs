// Actual Settings controls and native broker/MCP effects in an isolated QA profile.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { copyFile, mkdir, readFile, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { resolve } from "node:path";
import { connectNativeWebView } from "./native-webview.mjs";

const root = resolve(`src-tauri/target/native-mcp-approval-${Date.now()}`);
await mkdir(root, { recursive: true });
const executable = resolve(root, "aworkit-desktop.exe");
await copyFile(resolve(process.env.AWORKIT_QA_BINARY ?? "src-tauri/target/aworkit-mcp-approval-qa.exe"), executable);
const auditPath = resolve(root, "calls.jsonl");
await writeFile(auditPath, "");
const python = process.env.AWORKIT_QA_PYTHON ?? "C:\\Python313\\python.exe";
const requests = [], failures = [], checks = [];
let phase = "", toolName = "", toolTurns = 0, reviews = 0;
const provider = createServer(async (req, res) => {
  try {
    if (req.method !== "POST") return res.end(JSON.stringify({ data: [{ id: "approval-fixture" }] }));
    let raw = ""; for await (const chunk of req) raw += chunk;
    const body = JSON.parse(raw); requests.push(body);
    if (body.messages.some(m => typeof m.content === "string" && m.content.includes("independent approval reviewer"))) reviews++;
    const name = body.tools?.find(t => t.function.description === `Approval fixture ${toolName}`)?.function.name;
    const message = !name ? { role: "assistant", content: JSON.stringify({ goal: "Exercise the requested fixture tool", openQuestions: [], evidenceNeeded: [], toolOrder: [toolName] }) }
      : ++toolTurns === 1 ? { role: "assistant", content: null, tool_calls: [{ id: `call.${phase}`, type: "function",
        function: { name, arguments: JSON.stringify({ label: phase }) } }] }
      : { role: "assistant", content: `COMPLETED ${phase}` };
    const usage = { prompt_tokens: 30, completion_tokens: 10, total_tokens: 40 };
    if (body.stream) {
      const delta = message.tool_calls ? { ...message, tool_calls: message.tool_calls.map((call, index) => ({ ...call, index })) } : message;
      res.setHeader("Content-Type", "text/event-stream");
      for (const chunk of [
        { choices: [{ index: 0, delta, finish_reason: null }] },
        { choices: [{ index: 0, delta: {}, finish_reason: message.tool_calls ? "tool_calls" : "stop" }] },
        { choices: [], usage },
      ]) res.write(`data: ${JSON.stringify({ id: "fixture-response", object: "chat.completion.chunk", model: "approval-fixture", ...chunk })}\n\n`);
      res.end("data: [DONE]\n\n");
    } else res.end(JSON.stringify({ choices: [{ index: 0, message, finish_reason: message.tool_calls ? "tool_calls" : "stop" }], usage }));
  } catch (error) { failures.push(String(error)); res.statusCode = 500; res.end(String(error)); }
});
provider.listen(0, "127.0.0.1"); await once(provider, "listening");
const origin = `http://127.0.0.1:${provider.address().port}`;
const reservation = createServer(); reservation.listen(0, "127.0.0.1"); await once(reservation, "listening");
const port = reservation.address().port; await new Promise(r => reservation.close(r));
let child, view, logs = "";
const delay = () => new Promise(r => setTimeout(r, 100));
async function start() {
  child = spawn(executable, [], { windowsHide: true, stdio: ["ignore", "pipe", "pipe"], env: { ...process.env,
    AWORKIT_QA_PROFILE: root, AWORKIT_QA_HIDE_WINDOW: "1", WEBVIEW2_USER_DATA_FOLDER: resolve(root, "webview"),
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}` } });
  child.stdout.on("data", c => logs += c); child.stderr.on("data", c => logs += c);
  for (let i = 0; i < 200; i++) { try {
    view = await connectNativeWebView(`http://127.0.0.1:${port}`);
    const diagnostics = "window.__mcpApprovalErrors=[];window.addEventListener('error',e=>window.__mcpApprovalErrors.push(e.message));window.addEventListener('unhandledrejection',e=>window.__mcpApprovalErrors.push(String(e.reason)));";
    await view.command("Page.addScriptToEvaluateOnNewDocument", { source: diagnostics });
    await view.evaluate(diagnostics);
    return;
  } catch { await delay(); } }
  throw new Error(`Native WebView failed: ${logs}`);
}
async function stop() {
  view?.close(); view = undefined;
  if (child?.exitCode === null) {
    const end = once(child, "exit");
    if (process.platform === "win32") {
      const killer = spawn(resolve(process.env.SystemRoot ?? "C:/Windows", "System32/taskkill.exe"),
        ["/PID", String(child.pid), "/T", "/F"], { windowsHide: true, stdio: "ignore" });
      await once(killer, "exit");
    } else child.kill();
    await end;
  }
}
async function waitFor(expression, attempts = 250) {
  for (let i = 0; i < attempts; i++) {
    if (failures.length) throw new Error(failures.join("\n"));
    const errors = await view.evaluate("window.__mcpApprovalErrors ?? []");
    if (errors.length) throw new Error(errors.join("\n"));
    if (await view.evaluate(expression)) return;
    await delay();
  }
  const composer = await view.evaluate("document.querySelector('.composer-submit')?.outerHTML");
  throw new Error(`Timeout: ${expression}\n${composer}\n${(await view.evaluate("document.body.innerText")).slice(-5000)}`);
}
async function click(name) {
  const find = `(()=>{const n=${JSON.stringify(name)};return [...document.querySelectorAll('button')].find(b=>!b.disabled&&b.getClientRects().length&&(b.title===n||b.textContent.trim()===n||b.getAttribute('aria-label')===n||b.textContent.trim().startsWith(n)));})()`;
  await waitFor(`Boolean(${find})`); await view.evaluate(`(${find}).click()`);
}
async function setValue(selector, value) {
  await waitFor(`Boolean(document.querySelector(${JSON.stringify(selector + ":not(:disabled)")}))`);
  await view.evaluate(`(()=>{const e=document.querySelector(${JSON.stringify(selector)});Object.getOwnPropertyDescriptor(e.tagName==='SELECT'?HTMLSelectElement.prototype:HTMLTextAreaElement.prototype,'value').set.call(e,${JSON.stringify(value)});e.dispatchEvent(new Event(e.tagName==='SELECT'?'change':'input',{bubbles:true}));})()`);
}
const snapshot = () => view.evaluate("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0})");
const settings = () => view.evaluate("window.__TAURI_INTERNALS__.invoke('settings_v2_snapshot')");
const audit = async () => (await readFile(auditPath, "utf8")).trim().split("\n").filter(Boolean).map(JSON.parse);
const autoCheckbox = `(()=>{const d=[...document.querySelectorAll('details')].find(d=>d.querySelector('summary')?.textContent==='unannotated');return [...d.querySelectorAll('label')].find(l=>l.textContent.trim()==='Auto approve').querySelector('input');})()`;
async function openTool() {
  await click("Settings"); await click("MCP servers");
  await waitFor("[...document.querySelectorAll('details summary')].some(s=>s.textContent==='unannotated')");
  await view.evaluate("(()=>{const s=[...document.querySelectorAll('details summary')].find(s=>s.textContent==='unannotated');if(!s.parentElement.open)s.click();s.scrollIntoView({block:'center'});})()");
}
async function save() {
  await click("Save configuration");
  await waitFor("[...document.querySelectorAll('button')].some(b=>b.textContent.trim()==='Save configuration'&&b.disabled)");
}
async function send(label, tool, approval, fresh = true) {
  phase = label; toolName = tool; toolTurns = 0;
  const before = (await audit()).length, reviewBefore = reviews;
  if (fresh) {
    const previousState = await snapshot(), previous = previousState.chat.chatId;
    const created = await view.evaluate(`window.__TAURI_INTERNALS__.invoke('desktop_command',{command:${JSON.stringify({
      schemaVersion: 1, commandId: `mcp.approval.new.${label}`, expectedVersion: previousState.version,
      action: "new_chat", targetId: null, payload: {},
    })}}).catch(error=>({error:String(error)}))`);
    assert.equal(created.accepted, true, JSON.stringify(created));
    await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId!==${JSON.stringify(previous)}&&s.chat.phase==='draft')`);
    // A command sent outside the composer replaces the selected event stream;
    // reload the renderer so its subscription starts at this Chat's history.
    await view.command("Page.reload");
    await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke && document.querySelector('.composer-submit'))");
    await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>document.querySelector('.chat-view-header')?.textContent.includes(s.chat.runId))");
    await setValue('select[aria-label="Workflow for the first Chat input"]', "workflow.simple-chat");
  }
  const prompt = `Call ${tool} once for fixture ${label}.`;
  // Settings uses its real controls. Submit the fixture through the same native
  // command as the composer, with an exact history fence.
  const current = await snapshot();
  const receipt = await view.evaluate(`window.__TAURI_INTERNALS__.invoke('desktop_command',{command:${JSON.stringify({
    schemaVersion: 1, commandId: `mcp.approval.${label}`, expectedVersion: current.version,
    action: fresh ? "start" : "enqueue", targetId: null,
    payload: fresh ? { workflowId: "workflow.simple-chat", projectId: null, input: prompt, attachments: [] } : { input: prompt },
  })}}).catch(error=>({error:String(error)}))`);
  assert.equal(receipt.accepted, true, JSON.stringify(receipt));
  assert.ok(toolTurns > 0, `${label}: model received the configured MCP tool`);
  await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>['waiting_input','awaiting_approval','failed'].includes(s.chat.phase)&&s.chat.phase!=='draft')");
  const state = await snapshot();
  assert.equal(state.chat.phase, approval ? "awaiting_approval" : "waiting_input", `${label}: ${JSON.stringify(state.chat)}`);
  if (approval) {
    assert.equal((await audit()).length, before, "Unapproved MCP tool has not executed");
    await view.evaluate("document.querySelector('.timeline-scroll').scrollTop=document.querySelector('.timeline-scroll').scrollHeight");
    if (label === "partial-read") await view.screenshot(resolve(root, "standard-approval.png"));
    await waitFor("[...document.querySelectorAll('button')].some(b=>b.textContent.trim()==='Approve once'&&!b.disabled&&b.getClientRects().length)");
    const pending = await snapshot();
    const decisionId = pending.events.findLast(e => e.kind === "approval.requested").payload.decisionId;
    const decision = await view.evaluate(`window.__TAURI_INTERNALS__.invoke('desktop_command',{command:${JSON.stringify({
      schemaVersion: 1, commandId: `mcp.approval.allow.${label}`, expectedVersion: pending.version,
      action: "approval", targetId: null, payload: { decisionId, approved: true, choice: "approve_once" },
    })}}).catch(error=>({error:String(error)}))`);
    assert.equal(decision.accepted, true, JSON.stringify(decision));
    await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.phase==='waiting_input')");
  }
  await waitFor(`document.body.innerText.includes(${JSON.stringify(`COMPLETED ${label}`)})`);
  assert.deepEqual((await audit()).slice(before), [{ tool, label }]);
  assert.equal(reviews, reviewBefore, "Ask mode and auto-approved tools never invoke the reviewer");
  checks.push({ label, tool, prompted: approval, effects: 1 });
}

try {
  await start();
  await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke)");
  await view.evaluate(`(async()=>{const invoke=window.__TAURI_INTERNALS__.invoke;
    const s=await invoke('settings_snapshot');await invoke('settings_commit',{command:{commandId:'mcp.approval.provider',expectedVersion:s.version,appearance:'system',portableHistoryEnabled:false,provider:{baseUrl:${JSON.stringify(origin + "/v1")},model:'approval-fixture',credentialAction:'keep',apiKey:null}}});
    const v=await invoke('settings_v2_snapshot');for(const p of v.settings.providers)for(const m of p.models)m.capabilities=['text','tools'];
    v.settings.approvals.defaultMode='ask_for_approval';
    const server={id:'mcp.approval',name:'Approval fixture',enabled:true,autoConnect:false,transport:{transport:'stdio',command:${JSON.stringify(python)},args:[${JSON.stringify(resolve("scripts/fixtures/mcp-approval.py"))},${JSON.stringify(auditPath)}],cwd:null,env:[]}};
    const probe=await invoke('settings_v2_probe_mcp',{request:{server,draftFingerprint:'approval-fixture'}});
    server.tools=probe.tools.map(t=>({...t,options:{approvalMode:'ask_for_approval'}}));
    v.settings.mcpServers=[server];await invoke('settings_v2_commit',{command:{commandId:'mcp.approval.server',expectedVersion:v.version,settings:v.settings}});
    const w=await invoke('workflow_snapshot',{workflowId:'workflow.simple-chat'});w.document.nodes.find(n=>n.type==='agent').configuration.toolIds=['mcp:mcp.approval'];
    await invoke('workflow_commit',{command:{commandId:'mcp.approval.workflow',expectedVersion:w.version,workflowId:'workflow.simple-chat',document:w.document}});
    await invoke('workflow_set_default',{command:{commandId:'mcp.approval.default',workflowId:'workflow.simple-chat'}});})()`);
  await view.command("Page.reload");
  const catalog = (await settings()).settings.mcpServers[0].tools;
  assert.deepEqual(catalog.find(t => t.name === "read_safe").annotations, { readOnlyHint: true, destructiveHint: false });
  assert.deepEqual(catalog.find(t => t.name === "only_read").annotations, { readOnlyHint: true });
  assert.equal(catalog.find(t => t.name === "unannotated").annotations, undefined);
  await openTool();
  await view.evaluate(`(${autoCheckbox}).click()`);
  await waitFor("document.getElementById('mcp-auto-approve-title')?.textContent==='Enable Auto approve?'");
  assert.equal(await view.evaluate(`(${autoCheckbox}).checked`), false);
  await view.screenshot(resolve(root, "auto-approve-confirmation.png"));
  await click("Cancel");
  await waitFor("!document.getElementById('mcp-auto-approve-title')");
  assert.equal(await view.evaluate(`(${autoCheckbox}).checked`), false);
  await view.evaluate(`(${autoCheckbox}).click()`);
  await click("Confirm");
  await waitFor(`(${autoCheckbox}).checked`);
  await click("Refresh functions");
  await waitFor("document.body.innerText.includes('Connection successful')");
  assert.equal(await view.evaluate(`(${autoCheckbox}).checked`), true);
  await view.screenshot(resolve(root, "auto-approve-settings.png"));
  await save();
  assert.equal((await settings()).settings.mcpServers[0].tools.find(t => t.name === "unannotated").options.autoApprove, true);
  await click("Back to Chat");
  await send("safe", "read_safe", false);
  await send("partial-read", "only_read", true);
  await send("partial-destructive", "only_non_destructive", true);
  await send("write", "write_claim", true);
  await send("destructive", "destructive", true);
  await send("auto", "unannotated", false);
  const frozenChat = (await snapshot()).chat.chatId;
  await openTool(); await view.evaluate(`(${autoCheckbox}).click()`);
  assert.equal(await view.evaluate("Boolean(document.getElementById('mcp-auto-approve-title'))"), false);
  await save(); await click("Back to Chat");
  await send("frozen", "unannotated", false, false);
  await stop(); await start();
  await waitFor("Boolean(document.querySelector('textarea[aria-label=\"Chat input\"]'))");
  assert.equal((await snapshot()).chat.chatId, frozenChat);
  assert.equal((await settings()).settings.mcpServers[0].tools.find(t => t.name === "unannotated").options.autoApprove ?? false, false);
  await send("restart-frozen", "unannotated", false, false);
  await send("new-chat-off", "unannotated", true);
  await view.screenshot(resolve(root, "completed.png"));
  assert.deepEqual(failures, []);
  console.log(JSON.stringify({ root, checks, toolCalls: (await audit()).length, providerRequests: requests.length, reviews }, null, 2));
} catch (error) {
  if (view) { await view.screenshot(resolve(root, "failure.png")); await writeFile(resolve(root, "failure-ui.txt"), await view.evaluate("document.body.innerText")); }
  throw error;
} finally {
  await writeFile(resolve(root, "result.json"), JSON.stringify({ checks, failures, requests, logs }, null, 2));
  await stop(); await new Promise(r => provider.close(r));
}
