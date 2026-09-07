// Native regression against a real Adashi stdio executable and a local provider.
// Set AWORKIT_QA_ADASHI_EXE to the configured executable. Only a read-only rule
// lookup is called; Chat, workflow and Settings edits use an isolated QA profile.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { once } from "node:events";
import { isDeepStrictEqual } from "node:util";
import { connectNativeWebView } from "./native-webview.mjs";

assert.ok(process.env.AWORKIT_QA_ADASHI_EXE, "Set AWORKIT_QA_ADASHI_EXE to the real Adashi executable");
const root = resolve("src-tauri/target/native-adashi-chat-" + Date.now());
await mkdir(root, { recursive: true });
const requests = [];
let ruleSchema;
const provider = createServer(async (request, response) => {
  if (request.url === "/v1/models") return response.end(JSON.stringify({ data: [{ id: "adashi-test" }] }));
  const chunks = []; for await (const chunk of request) chunks.push(chunk);
  const body = JSON.parse(Buffer.concat(chunks).toString()); requests.push(body);
  const result = body.messages.find(message => message.role === "tool");
  // Select the read-only rule lookup by its schema: the provider alias may
  // truncate the display prefix, while dispatch must retain the exact MCP name.
  const matches = body.tools?.filter(tool => isDeepStrictEqual(tool.function.parameters, ruleSchema)) ?? [];
  if (body.tools?.length && matches.length !== 1) {
    response.writeHead(400); return response.end("The exact read-only Adashi schema was not unique");
  }
  const name = matches[0]?.function.name;
  const message = !name ? { role: "assistant", content: JSON.stringify({ goal: "Inspect Adashi's available rules.", openQuestions: [], evidenceNeeded: ["Aworkit rules"], toolOrder: ["adashi_get_rule_injections"] }) }
    : result ? { role: "assistant", content: "Adashi returned the Aworkit rule lookup." }
    : { role: "assistant", content: null, tool_calls: [{ id: "adashi.read.rules", type: "function", function: {
      name, arguments: JSON.stringify({ projectId: "Aworkit", intend: "general", hook: "task.start" }),
    } }] };
  const usage = { prompt_tokens: 20, completion_tokens: 10, total_tokens: 30 };
  if (body.stream) {
    response.setHeader("Content-Type", "text/event-stream");
    const delta = message.tool_calls ? { tool_calls: message.tool_calls.map((call, index) => ({ index, ...call })) } : message;
    for (const chunk of [{ choices: [{ index: 0, delta, finish_reason: null }] },
      { choices: [{ index: 0, delta: {}, finish_reason: message.tool_calls ? "tool_calls" : "stop" }] }, { choices: [], usage }]) {
      response.write("data: " + JSON.stringify({ id: "adashi-test", model: "adashi-test", ...chunk }) + "\n\n");
    }
    response.end("data: [DONE]\n\n");
  } else response.end(JSON.stringify({ id: "adashi-test", model: "adashi-test", choices: [{ index: 0, message,
    finish_reason: message.tool_calls ? "tool_calls" : "stop" }], usage }));
});
provider.listen(0, "127.0.0.1"); await once(provider, "listening");
const origin = "http://127.0.0.1:" + provider.address().port;
const reservation = createServer(); reservation.listen(0, "127.0.0.1"); await once(reservation, "listening");
const debuggerPort = reservation.address().port; await new Promise(resolve => reservation.close(resolve));
const child = spawn(resolve(process.env.AWORKIT_QA_EXE ?? "src-tauri/target/debug/aworkit-desktop.exe"), [], {
  windowsHide: true, stdio: "ignore", env: { ...process.env, AWORKIT_QA_PROFILE: root,
    WEBVIEW2_USER_DATA_FOLDER: resolve(root, "webview"), AWORKIT_QA_HIDE_WINDOW: "1",
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: "--remote-debugging-port=" + debuggerPort },
});
let view;
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
async function waitFor(expression) {
  for (let i = 0; i < 200; i++) {
    if (await view.evaluate(expression)) return;
    await pause(100);
  }
  throw new Error("Timed out: " + expression + "\n" + await view.evaluate("document.body.innerText"));
}
try {
  for (let i = 0; i < 100; i++) {
    try { view = await connectNativeWebView("http://127.0.0.1:" + debuggerPort, process.env.AWORKIT_QA_PAGE_URL ?? "http://tauri.localhost"); break; }
    catch { await pause(150); }
  }
  assert.ok(view, "Native WebView must start");
  await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke)");
  const server = { id: "mcp.166dddff4b6840dba8aed1edbbb9e427", name: "Adashi", enabled: true, autoConnect: false,
    transport: { transport: "stdio", command: resolve(process.env.AWORKIT_QA_ADASHI_EXE), args: [],
      cwd: dirname(resolve(process.env.AWORKIT_QA_ADASHI_EXE)), env: [] } };
  const catalog = await view.evaluate(`window.__TAURI_INTERNALS__.invoke('settings_v2_probe_mcp', {request:{server:${JSON.stringify(server)},draftFingerprint:'adashi.qa'}})`);
  assert.ok(catalog.tools.length > 1);
  ruleSchema = catalog.tools.find(tool => tool.name === 'adashi_get_rule_injections')?.inputSchema;
  assert.ok(ruleSchema, "The exact read-only function must be present before starting a Chat");
  server.tools = catalog.tools;
  await view.evaluate(`(async()=>{const invoke=window.__TAURI_INTERNALS__.invoke;const s=await invoke('settings_snapshot');
    await invoke('settings_commit',{command:{commandId:'adashi.qa.provider',expectedVersion:s.version,appearance:'system',portableHistoryEnabled:false,
      provider:{baseUrl:${JSON.stringify(origin + "/v1")},model:'adashi-test',credentialAction:'keep',apiKey:null}}});
    const v=await invoke('settings_v2_snapshot');for(const p of v.settings.providers)for(const m of p.models)m.capabilities=['text','tools'];
    v.settings.mcpServers=[${JSON.stringify(server)}];
    await invoke('settings_v2_commit',{command:{commandId:'adashi.qa.settings',expectedVersion:v.version,settings:v.settings}});
    const w=await invoke('workflow_snapshot',{workflowId:'workflow.standard-agent'});
    w.document.nodes.find(n=>n.type==='agent').configuration.toolIds=['mcp:'+${JSON.stringify(server.id)}];
    await invoke('workflow_commit',{command:{commandId:'adashi.qa.workflow',expectedVersion:w.version,workflowId:'workflow.standard-agent',document:w.document}});
  })()`);
  await view.command("Page.reload");
  await waitFor("Boolean(document.querySelector('textarea[aria-label=\"Chat input\"]'))");
  for (const [selector, value] of [['select[aria-label="Workflow for the first Chat input"]', 'workflow.standard-agent'],
    ['select[aria-label="Approval mode"]', 'full_access'], ['textarea[aria-label="Chat input"]', 'Read the Aworkit rules from Adashi.']]) {
    await view.evaluate(`(() => {const e=document.querySelector(${JSON.stringify(selector)});const p=e.tagName==='SELECT'?HTMLSelectElement.prototype:HTMLTextAreaElement.prototype;
      Object.getOwnPropertyDescriptor(p,'value').set.call(e,${JSON.stringify(value)});e.dispatchEvent(new Event(e.tagName==='SELECT'?'change':'input',{bubbles:true}));})()`);
  }
  await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.approvalMode==='full_access')");
  await waitFor("[...document.querySelectorAll('button')].some(b=>b.textContent.trim()==='Send'&&!b.disabled)");
  await view.evaluate("[...document.querySelectorAll('button')].find(b=>b.textContent.trim()==='Send').click()");
  await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.phase==='waiting_input')");
  const turns = requests.filter(r => r.tools?.length);
  assert.equal(turns.length, 2, "Agent sends one MCP call and consumes its result");
  for (const turn of turns) {
    const names = turn.tools.map(t => t.function.name);
    assert.equal(names.length, catalog.tools.length, "Every selected Adashi function reaches the provider");
    assert.equal(new Set(names).size, names.length);
    assert.ok(names.every(name => /^[A-Za-z0-9_-]{1,64}$/.test(name)));
  }
  const result = turns[1].messages.find(m => m.role === "tool");
  const delivered = JSON.parse(result.content);
  assert.equal(delivered.tool, 'adashi_get_rule_injections', "Alias dispatch retains the exact original MCP name");
  assert.equal(delivered.result.isError, false, "Adashi reports a successful lookup");
  assert.ok(result.content.includes('injectionPrompt') && result.content.includes('Aworkit'), "Real Adashi result reaches the continuation");
  await view.screenshot(resolve(root, "chat.png"));
  const report = { ok: true, root, transport: "stdio", provider: "local deterministic fixture", toolCount: catalog.tools.length,
    maximumNameLength: Math.max(...turns[0].tools.map(t=>t.function.name.length)), providerCalls: turns.length,
    calledTool: "adashi_get_rule_injections", project: "Aworkit" };
  await writeFile(resolve(root, "report.json"), JSON.stringify(report, null, 2));
  console.log(JSON.stringify(report));
} catch (error) {
  await writeFile(resolve(root, "failure.txt"), String(error));
  await view?.screenshot(resolve(root, "failure.png")).catch(()=>{});
  throw error;
} finally { view?.close(); child.kill(); provider.close(); }
