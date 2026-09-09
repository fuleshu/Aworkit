// Native editor -> frozen MCP -> provider payload, using isolated deterministic fixtures.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { copyFile, mkdir, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { resolve } from "node:path";
import { connectNativeWebView } from "./native-webview.mjs";

const root = resolve(`src-tauri/target/native-context-efficiency-${Date.now()}`);
const project = resolve(root, "project");
await mkdir(project, { recursive: true });
const executable = resolve(root, "aworkit-desktop.exe");
await copyFile(resolve(process.env.AWORKIT_QA_BINARY ?? "src-tauri/target/debug/aworkit-desktop.exe"), executable);
const custom = "Preserve the fixture receipt when describing this result.";
const descriptions = ["List fixture tasks filtered by state.", "Read complete fixture task evidence."];
const requests = [], failures = [], measurements = [];
let phase = "lookup", phaseTurn = 0, firstPreview, reference, catalog;
function call(name, args) {
  return { tool_calls: [{ index: 0, id: `efficiency.${phase}.${phaseTurn}`, type: "function", function: { name, arguments: JSON.stringify(args) } }] };
}
const provider = createServer(async (req, res) => {
  try {
    if (req.method !== "POST") return res.end(JSON.stringify({ data: [{ id: "efficiency" }] }));
    let raw = ""; for await (const chunk of req) raw += chunk;
    const body = JSON.parse(raw); requests.push(body); phaseTurn++;
    const system = body.messages.filter(m => m.role === "system").map(m => m.content).join("\n");
    for (const description of descriptions) {
      assert.ok(!system.includes(description), "MCP descriptions belong only to tool definitions");
      assert.equal(body.tools.filter(t => t.function.description === description).length, 1);
    }
    assert.ok(system.includes(custom), "Explicit custom instructions survive");
    assert.ok(system.includes("including an empty result, is sufficient evidence"));
    assert.ok(system.includes("Treat truncated or paginated previews as incomplete"));
    const lookupName = system.match(/^([A-Za-z0-9_-]+) = mcp:\/\/mcp.efficiency\/list_tasks$/m)?.[1];
    const largeName = system.match(/^Tool ([A-Za-z0-9_-]+) \(mcp:\/\/mcp.efficiency\/large_result\):$/m)?.[1];
    assert.ok(lookupName && largeName, "Both identity-only and custom-instruction bindings resolve");
    const results = body.messages.filter(m => m.role === "tool");
    let message;
    if (phase === "lookup") {
      if (phaseTurn === 1) message = call(lookupName, { projectId: "Aworkit", states: ["open"] });
      else {
        assert.equal(phaseTurn, 2);
        const result = JSON.parse(results.at(-1).content);
        assert.deepEqual(result.result.structuredContent.tasks, []);
        assert.equal(result.result.structuredContent.hasMore, false);
        assert.deepEqual(result.result.content, [], "Compatibility JSON copy is still removed");
        message = { content: "No — there are no open tasks for Aworkit in Adashi." };
      }
    } else if (phase === "preview" || phase === "retrievable") {
      if (phaseTurn === 1) message = call(largeName, {});
      else {
        assert.equal(phaseTurn, 2);
        const text = results.at(-1).content, result = JSON.parse(text);
        assert.ok(Buffer.byteLength(text) <= 4096);
        assert.equal(result.aworkitOutput.truncated, true);
        assert.ok(result.aworkitOutput.originalBytes > 200000);
        assert.equal(result.preview.result.isError, false);
        assert.equal(result.preview.result.structuredContent.tail, "FINAL_TASK_MARKER");
        const blocks = result.preview.result.content;
        assert.ok(blocks.some(b => b.type === "image" && b.data.startsWith("iVBOR")));
        assert.ok(blocks.some(b => b.text === "Distinct annotated explanation." && b.annotations.audience[0] === "user"));
        assert.ok(!text.includes("OMITTED_RECEIPT_739"));
        measurements.push({ phase, originalBytes: result.aworkitOutput.originalBytes, deliveredBytes: Buffer.byteLength(text), systemChars: system.length });
        if (phase === "preview") { firstPreview = text; assert.ok(!result.aworkitContext); }
        else { reference = result.aworkitContext.reference; assert.equal(reference.length, 64); }
        message = { content: phase === "preview" ? "VALID PREVIEW VERIFIED" : "RETRIEVABLE PREVIEW VERIFIED" };
      }
    } else if (phase === "reopen") {
      assert.equal(phaseTurn, 1);
      assert.ok(results.some(r => r.content === firstPreview), "Prior preview survives restart unchanged");
      message = { content: "RESTART PREVIEW VERIFIED" };
    } else if (phase === "recover") {
      if (phaseTurn === 1) message = call("aworkit_context", { operation: "search", reference, pointer: "/result/structuredContent/tasks/39/description", query: "OMITTED_RECEIPT_739" });
      else {
        assert.equal(phaseTurn, 2);
        assert.ok(results.at(-1).content.includes("OMITTED_RECEIPT_739"));
        message = { content: "EXACT ORIGINAL RECOVERED" };
      }
    } else throw new Error(`Unexpected phase ${phase}`);
    res.setHeader("Content-Type", "text/event-stream");
    res.end(`data: ${JSON.stringify({ choices: [{ index: 0, delta: message, finish_reason: null }] })}\n\n` +
      `data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: message.tool_calls ? "tool_calls" : "stop" }], usage: { prompt_tokens: 1000, completion_tokens: 50 } })}\n\n` + "data: [DONE]\n\n");
  } catch (error) { failures.push(String(error)); res.statusCode = 500; res.end(String(error)); }
});
provider.listen(0, "127.0.0.1"); await once(provider, "listening");
const origin = `http://127.0.0.1:${provider.address().port}`;
const reservation = createServer(); reservation.listen(0, "127.0.0.1"); await once(reservation, "listening");
const debuggerPort = reservation.address().port; await new Promise(r => reservation.close(r));
let child, view, logs = "";
const delay = () => new Promise(r => setTimeout(r, 100));
async function start() {
  child = spawn(executable, [], { windowsHide: true, stdio: ["ignore", "pipe", "pipe"], env: { ...process.env,
    AWORKIT_QA_PROFILE: root, AWORKIT_QA_HIDE_WINDOW: "1", WEBVIEW2_USER_DATA_FOLDER: resolve(root, "webview"),
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${debuggerPort}` } });
  child.stdout.on("data", c => logs += c); child.stderr.on("data", c => logs += c);
  for (let i = 0; i < 200; i++) { try { view = await connectNativeWebView(`http://127.0.0.1:${debuggerPort}`); return; } catch { await delay(); } }
  throw new Error(`Native WebView failed: ${logs}`);
}
async function stop() {
  view?.close(); view = undefined;
  if (child?.exitCode === null) {
    const end = once(child, "exit");
    // Stop only this fixture's tracked process tree, including its MCP and
    // WebView children, before reusing the isolated profile and debugger port.
    if (process.platform === "win32") {
      const killer = spawn(resolve(process.env.SystemRoot ?? "C:/Windows", "System32/taskkill.exe"),
        ["/PID", String(child.pid), "/T", "/F"], { windowsHide: true, stdio: "ignore" });
      await once(killer, "exit");
    } else child.kill();
    await end;
  }
}
async function waitFor(expression) {
  for (let i = 0; i < 300; i++) { if (failures.length) throw new Error(failures.join("\n")); if (await view.evaluate(expression)) return; await delay(); }
  throw new Error(`Timeout: ${expression}\n${(await view.evaluate("document.body.innerText")).slice(-5000)}`);
}
async function click(name) {
  const find = `(()=>{const n=${JSON.stringify(name)};return [...document.querySelectorAll('button')].find(b=>!b.disabled&&b.getClientRects().length&&(b.title===n||b.textContent.trim()===n||b.textContent.trim().startsWith(n+' ')||b.getAttribute('aria-label')===n));})()`;
  await waitFor(`Boolean(${find})`); await view.evaluate(`(${find}).click()`);
}
async function setValue(selector, value) {
  await waitFor(`Boolean(document.querySelector(${JSON.stringify(selector)}))`);
  await view.evaluate(`(()=>{const e=document.querySelector(${JSON.stringify(selector)});if(e.value===${JSON.stringify(value)})return;Object.getOwnPropertyDescriptor(e.tagName==='SELECT'?HTMLSelectElement.prototype:HTMLTextAreaElement.prototype,'value').set.call(e,${JSON.stringify(value)});e.dispatchEvent(new Event(e.tagName==='SELECT'?'change':'input',{bubbles:true}));})()`);
}
async function bindInEditor(selector) {
  // Workflow loading can reset the properties pane after the selector changes.
  // Select the actual Agent and bind only once the intended checkbox is present.
  await waitFor(`(()=>{const e=document.querySelector(${JSON.stringify(selector)});if(e){if(!e.checked)e.click();return true;}[...document.querySelectorAll('[aria-label="Workflow nodes"] button')].find(b=>b.textContent==='Agent')?.click();return false;})()`);
}
const snapshot = () => view.evaluate("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0})");
async function runWorkflow() {
  const prior = (await snapshot()).chat.chatId;
  await click("Run");
  await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId!==${JSON.stringify(prior)}&&document.body.innerText.includes(s.chat.runId)&&!document.querySelector('.workflow-library-bar')?.getClientRects().length)`);
}
async function send(nextPhase, prompt, answer) {
  phase = nextPhase; phaseTurn = 0;
  // A settled native event can precede the composer's committed draft reset.
  await waitFor(`(()=>{const e=document.querySelector('textarea[aria-label="Chat input"]');if(!e||e.disabled)return false;if(e.value!==${JSON.stringify(prompt)}){Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set.call(e,${JSON.stringify(prompt)});e.dispatchEvent(new Event('input',{bubbles:true}));return false;}return Boolean(document.querySelector('.composer-input .primary-action:not(:disabled)'));})()`);
  await view.evaluate("document.querySelector('.composer-input .primary-action').click()");
  await waitFor(`document.querySelector('.timeline-scroll')?.textContent.includes(${JSON.stringify(answer)})`);
  await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.phase==='waiting_input')");
}
try {
  await start(); await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke)");
  const server = { id: "mcp.efficiency", name: "Efficiency fixture", enabled: true, autoConnect: false,
    transport: { transport: "stdio", command: process.env.AWORKIT_QA_PYTHON ?? "C:\\Python313\\python.exe",
      args: ["-X", "utf8", resolve("scripts/fixtures/mcp-context-efficiency.py")], env: [] } };
  catalog = await view.evaluate(`window.__TAURI_INTERNALS__.invoke('settings_v2_probe_mcp',{request:{server:${JSON.stringify(server)},draftFingerprint:'efficiency.fixture'}})`);
  server.tools = catalog.tools;
  server.tools.find(t => t.name === "large_result").options = { instructions: custom };
  await view.evaluate(`(async()=>{const invoke=window.__TAURI_INTERNALS__.invoke,s=await invoke('settings_snapshot');
    await invoke('settings_commit',{command:{commandId:'efficiency.provider',expectedVersion:s.version,appearance:'light',portableHistoryEnabled:false,provider:{baseUrl:${JSON.stringify(origin + "/v1")},model:'efficiency',credentialAction:'keep',apiKey:null}}});
    const v=await invoke('settings_v2_snapshot');for(const p of v.settings.providers){p.configuration.maximumToolOutputBytes=4096;for(const m of p.models){m.capabilities=['text','tools'];m.contextWindow=100000;m.compaction={auto:false,pruneToolResults:false,compression:{mode:'off'}};}}
    v.settings.approvals.defaultMode='full_access';v.settings.mcpServers=[${JSON.stringify(server)}];v.settings.tools.find(t=>t.id==='tool.context').enabled=true;
    v.settings.projects=[{id:'project.efficiency',name:'Aworkit',workspace:{kind:'local_directory',location:${JSON.stringify(project)}},defaultWorkflowId:'workflow.simple-chat',portableHistoryEnabled:false}];
    await invoke('settings_v2_commit',{command:{commandId:'efficiency.settings',expectedVersion:v.version,settings:v.settings}});
    await invoke('workflow_set_default',{command:{commandId:'efficiency.default',workflowId:'workflow.simple-chat'}});
  })()`);
  await view.command("Page.reload"); await click("Workflows"); await setValue(".workflow-library-bar select", "workflow.simple-chat");
  const selector = 'input[title="Use all enabled Efficiency fixture functions in this Agent"]';
  await bindInEditor(selector);
  await click("Validate"); await waitFor("document.body.innerText.includes('Validation passed: this workflow document is executable.')");
  await view.screenshot(resolve(root, "workflow-validation.png")); await click("Save");
  await waitFor("window.__TAURI_INTERNALS__.invoke('workflow_snapshot',{workflowId:'workflow.simple-chat'}).then(w=>w.document.nodes.find(n=>n.type==='agent').configuration.toolIds.includes('mcp:mcp.efficiency'))");
  await runWorkflow(); await setValue('select[aria-label="Workflow for the first Chat input"]', "workflow.simple-chat");
  await setValue('select[aria-label="Project for the first Chat input"]', "project.efficiency");
  await send("lookup", "are there any open tasks for aworkit in adashi?", "there are no open tasks");
  await send("preview", "Read the large fixture result.", "VALID PREVIEW VERIFIED");
  await view.screenshot(resolve(root, "valid-preview.png"));
  await stop(); await start(); await send("reopen", "Confirm that the earlier preview is still available.", "RESTART PREVIEW VERIFIED");
  await view.evaluate(`(async()=>{const invoke=window.__TAURI_INTERNALS__.invoke,v=await invoke('settings_v2_snapshot');for(const p of v.settings.providers)for(const m of p.models)m.compaction.compression.mode='lossless';await invoke('settings_v2_commit',{command:{commandId:'efficiency.retrieval-settings',expectedVersion:v.version,settings:v.settings}});})()`);
  await click("Workflows"); await setValue(".workflow-library-bar select", "workflow.simple-chat");
  await bindInEditor('input[title="Bind Context retrieval to this agent"]');
  await click("Validate"); await waitFor("document.body.innerText.includes('Validation passed: this workflow document is executable.')"); await click("Save");
  await waitFor("window.__TAURI_INTERNALS__.invoke('workflow_snapshot',{workflowId:'workflow.simple-chat'}).then(w=>w.document.nodes.find(n=>n.type==='agent').configuration.toolIds.includes('tool.context'))");
  await runWorkflow(); await setValue('select[aria-label="Workflow for the first Chat input"]', "workflow.simple-chat");
  await setValue('select[aria-label="Project for the first Chat input"]', "project.efficiency");
  await send("retrievable", "Read the large fixture result with original retrieval available.", "RETRIEVABLE PREVIEW VERIFIED");
  const state = await snapshot(), archive = state.events.find(e => e.kind === "context.compression");
  assert.ok(archive); assert.ok(JSON.stringify(archive.payload.original).includes("OMITTED_RECEIPT_739"));
  assert.deepEqual(archive.payload.metrics.strategies, ["output-limit-preview"]);
  await stop(); await start(); await send("recover", "Recover the omitted receipt from the original result.", "EXACT ORIGINAL RECOVERED");
  await view.screenshot(resolve(root, "retrieval-after-restart.png"));
  assert.deepEqual(failures, []);
  const report = { ok: true, root, provider: "deterministic loopback fixture; not a live model behavior evaluation", measurements,
    cases: ["native editor Validate Save Run", "single MCP description and preserved custom instructions", "filtered empty query scenario", "structured cap with complete metadata and media", "stable preview after restart", "durable original and exact omitted receipt retrieval after restart"] };
  await writeFile(resolve(root, "report.json"), JSON.stringify(report, null, 2)); console.log(JSON.stringify(report, null, 2));
} catch (error) {
  if (view) { await writeFile(resolve(root, "failure-snapshot.json"), JSON.stringify(await snapshot(), null, 2)); await view.screenshot(resolve(root, "failure.png")); }
  console.error("Evidence:", root); throw error;
} finally { await stop(); await writeFile(resolve(root, "requests.json"), JSON.stringify(requests, null, 2)); await writeFile(resolve(root, "native.log"), logs); provider.closeAllConnections(); provider.close(); }
