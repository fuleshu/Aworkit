// Regression for different Python scripts after Always approve in project.
// Real native controls, broker, Python effects and restart migration; local fixture only.
import { createServer } from "node:http";
import { spawn, execFileSync } from "node:child_process";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { once } from "node:events";
import assert from "node:assert/strict";
import { connectNativeWebView } from "./native-webview.mjs";

const root = resolve(`src-tauri/target/native-project-approval-${Date.now()}`);
const project = resolve(root, "project");
await mkdir(project, { recursive: true });
const python = process.env.AWORKIT_QA_PYTHON ?? "C:\\Python313\\python.exe";
let scenario = "initial";
const requests = [];
const provider = createServer(async (request, response) => {
  if (request.url === "/v1/models") return response.end(JSON.stringify({ data: [{ id: "approval-fixture" }] }));
  const chunks = []; for await (const chunk of request) chunks.push(chunk);
  const body = JSON.parse(Buffer.concat(chunks).toString()); requests.push(body);
  const step = body.messages.filter(message => message.role === "tool").length;
  const name = body.tools?.find(tool => tool.function.name.includes("python"))?.function.name;
  const script = `from pathlib import Path\nPath('${scenario}-${step}.txt').write_text('script ${step} executed', encoding='utf-8')\nprint(${step})`;
  const message = step >= 3 ? { role: "assistant", content: "All three different scripts completed." }
    : { role: "assistant", content: null, tool_calls: [{ id: `${scenario}.${step}`, type: "function", function: { name, arguments: JSON.stringify({ script }) } }] };
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
  } else response.end(JSON.stringify({ choices: [{ index: 0, message, finish_reason: message.tool_calls ? "tool_calls" : "stop" }], usage }));
});
provider.listen(0, "127.0.0.1"); await once(provider, "listening");
const origin = `http://127.0.0.1:${provider.address().port}`;
const port = 9261;
const launch = () => spawn(resolve("src-tauri/target/debug/aworkit-desktop.exe"), [], {
  windowsHide: true, stdio: "ignore", env: { ...process.env, AWORKIT_QA_PROFILE: root,
    AWORKIT_QA_HIDE_WINDOW: "1", WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}` },
});
let child = launch(), view;
const pause = () => new Promise(resolve => setTimeout(resolve, 100));
async function connect() {
  for (let i = 0; i < 150; i++) {
    try { return await connectNativeWebView(`http://127.0.0.1:${port}`); } catch {}
    await pause();
  }
  throw new Error("Native WebView did not start");
}
const evaluate = expression => view.evaluate(expression);
async function waitFor(expression) {
  for (let i = 0; i < 200; i++) { if (await evaluate(expression)) return; await pause(); }
  throw new Error(`Timed out: ${expression}\n${await evaluate("document.body.innerText")}`);
}
const click = name => waitFor(`(() => { const b=[...document.querySelectorAll('button')].find(b=>!b.disabled&&(b.title===${JSON.stringify(name)}||b.getAttribute('aria-label')===${JSON.stringify(name)}||b.textContent.trim()===${JSON.stringify(name)}||b.textContent.trim().replace(/^[＋⚙◇]\\s*/,'').startsWith(${JSON.stringify(name)}))); if(!b)return false;b.click();return true;})()`);
async function setValue(selector, value) {
  await waitFor(`Boolean(document.querySelector(${JSON.stringify(selector + ":not(:disabled)")}))`);
  await evaluate(`(() => {const e=document.querySelector(${JSON.stringify(selector)});const p=e.tagName==='SELECT'?HTMLSelectElement.prototype:HTMLTextAreaElement.prototype;Object.getOwnPropertyDescriptor(p,'value').set.call(e,${JSON.stringify(value)});e.dispatchEvent(new Event(e.tagName==='SELECT'?'change':'input',{bubbles:true}));})()`);
}
const snapshot = () => evaluate("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0})");
const waiting = () => waitFor("(() => { const scroll=document.querySelector('.timeline-scroll'); if(scroll)scroll.scrollTop=scroll.scrollHeight; return [...document.querySelectorAll('button')].some(b=>b.textContent==='Approve once'&&!b.disabled); })()");
const settled = () => waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.phase==='waiting_input')");
async function start(label) {
  scenario = label;
  const previous = (await snapshot()).chat.chatId;
  await click("New Chat");
  await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId!==${JSON.stringify(previous)}&&s.chat.phase==='draft')`);
  await setValue('select[aria-label="Workflow for the first Chat input"]', "workflow.simple-chat");
  await setValue('select[aria-label="Project for the first Chat input"]', "project.approval");
  await setValue('textarea[aria-label="Chat input"]', "Run three different Python scripts that write numbered files in this project.");
  await click("Send");
}
async function checkFiles(label) {
  for (let step = 0; step < 3; step++) assert.equal(await readFile(resolve(project, `${label}-${step}.txt`), "utf8"), `script ${step} executed`);
}
async function close() {
  view?.close(); view = undefined;
  if (child.exitCode === null) { const exited = once(child, "exit"); child.kill(); await exited; }
}
try {
  view = await connect();
  await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke)");
  await evaluate(`(async()=>{const invoke=window.__TAURI_INTERNALS__.invoke;
    const s=await invoke('settings_snapshot');await invoke('settings_commit',{command:{commandId:'project.approval.provider',expectedVersion:s.version,appearance:'system',portableHistoryEnabled:false,provider:{baseUrl:${JSON.stringify(origin + "/v1")},model:'approval-fixture',credentialAction:'keep',apiKey:null}}});
    const v=await invoke('settings_v2_snapshot');for(const p of v.settings.providers)for(const m of p.models)m.capabilities=['text','tools'];
    for(const tool of v.settings.tools)tool.enabled=true;
    v.settings.tools.find(t=>t.id==='tool.python.host').options={executable:${JSON.stringify(python)}};
    v.settings.approvals.defaultMode='ask_for_approval';
    v.settings.projects=[{id:'project.approval',name:'Python approval fixture',workspace:{kind:'local_directory',location:${JSON.stringify(project)}},defaultWorkflowId:'workflow.simple-chat',portableHistoryEnabled:false}];
    await invoke('settings_v2_commit',{command:{commandId:'project.approval.tools',expectedVersion:v.version,settings:v.settings}});
    const w=await invoke('workflow_snapshot',{workflowId:'workflow.simple-chat'});w.document.nodes.find(n=>n.type==='agent').configuration.toolIds=['tool.python.host'];
    await invoke('workflow_commit',{command:{commandId:'project.approval.workflow',expectedVersion:w.version,workflowId:'workflow.simple-chat',document:w.document}});})()`);
  await view.command("Page.reload");
  await start("initial"); await waiting();
  await click("Approve once");
  await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.events.filter(e=>e.kind==='approval.requested').length===2&&s.chat.phase==='awaiting_approval')");
  await waiting();
  assert.ok(await evaluate("document.body.innerText.includes('python scripts in this project')"));
  await view.screenshot(resolve(root, "python-approval.png"));
  await click("Always approve in project"); await settled(); await checkFiles("initial");
  assert.equal((await snapshot()).events.filter(e=>e.kind==="approval.requested").length, 2);
  await start("reuse"); await settled(); await checkFiles("reuse");
  assert.equal((await snapshot()).events.filter(e=>e.kind==="approval.requested").length, 0);

  // Simulate the previously shipped exact-script grant format in the isolated
  // profile, then reopen it through normal native startup and run changed scripts.
  await close();
  execFileSync(python, ["-c", `import hashlib,json,sqlite3,sys
db=sqlite3.connect(sys.argv[1])
grant=json.loads(db.execute('SELECT body FROM approval_project_grants').fetchone()[0])
db.execute('DELETE FROM approval_project_grants')
digest=lambda value:hashlib.sha256(json.dumps(value,sort_keys=True,separators=(',',':')).encode()).hexdigest()
for script in ['print(1)','print(2)']:
 grant['scope']='This exact action in this project'
 grant['actionSummary']=script
 grant['actionHash']=digest({'script':script})
 grant['id']=digest([grant['projectKey'],grant['bindingHash'],grant['actionHash']])
 db.execute('INSERT INTO approval_project_grants VALUES (?,?)',(grant['id'],json.dumps(grant)))
db.commit()
db.close()`, resolve(root, "runtime/history/aworkit-invocations.sqlite3")], { windowsHide: true });
  child = launch(); view = await connect();
  await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke)");
  const grants = await evaluate("window.__TAURI_INTERNALS__.invoke('approval_project_grants')");
  assert.equal(grants.length, 1); assert.equal(grants[0].scope, "Python scripts in this project");
  await start("migrated"); await settled(); await checkFiles("migrated");
  assert.equal((await snapshot()).events.filter(e=>e.kind==="approval.requested").length, 0);
  await click("Settings"); await click("Approvals");
  await waitFor("document.body.innerText.includes('including different arguments')");
  await view.screenshot(resolve(root, "saved-python-approval.png"));
  await click("Revoke approval"); await waitFor("document.body.innerText.includes('No saved project approvals.')");
  await click("Back to Chat");
  await start("revoked");
  for (let step = 0; step < 3; step++) {
    await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.events.filter(e=>e.kind==='approval.requested').length===${step + 1}&&s.chat.phase==='awaiting_approval')`);
    await waiting(); await click("Approve once");
  }
  await settled(); await checkFiles("revoked");
  const report = { ok: true, root, requests: requests.length, scriptsExecuted: 12,
    cases: ["approve once asks again", "always allows different scripts in same run", "reuse across chats", "legacy migration across restart", "revocation asks for every script"] };
  await writeFile(resolve(root, "result.json"), JSON.stringify(report, null, 2));
  console.log(JSON.stringify(report));
} finally { await close(); provider.close(); }
