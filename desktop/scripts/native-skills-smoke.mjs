// Native skill catalog, lazy loading, human invocation, Settings, and freeze proof.
// Uses an isolated profile and local provider; never touches the user's profile.
import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { mkdir, writeFile, unlink } from "node:fs/promises";
import { resolve } from "node:path";
import { once } from "node:events";
import assert from "node:assert/strict";
import { connectNativeWebView } from "./native-webview.mjs";

const root = resolve(`src-tauri/target/native-skills-${Date.now()}`);
const project = resolve(root, "project");
const skillDir = resolve(project, ".aworkit/skills");
await mkdir(skillDir, { recursive: true });
// The fixture lives inside Aworkit's checkout; give it its own project marker.
await mkdir(resolve(project, ".git"), { recursive: true });
await writeFile(resolve(project, ".git/HEAD"), "ref: refs/heads/main\n");
const writeSkill = (directory, name, description, body, flags = "") => writeFile(resolve(directory, `${name}.md`), `---\nname: ${name}\ndescription: ${description}\n${flags}---\n${body}\n`);
await writeSkill(skillDir, "example", "Initial example description", "PROJECT BODY V1");
await writeSkill(skillDir, "manual", "Manual only", "HUMAN INVOCATION BODY", "disable-model-invocation: true\n");
const globalHome = resolve(root, "global"); await mkdir(resolve(globalHome, "skills"), { recursive: true });
await writeSkill(resolve(globalHome, "skills"), "example", "Global fallback", "GLOBAL BODY");
await mkdir(resolve(project, ".dsh/skills"), { recursive: true });
await writeSkill(resolve(project, ".dsh/skills"), "ignored", "Must be ignored", "DEEPSEEK BODY");
const requests = []; const failures = [];
const provider = createServer(async (request, response) => {
  try {
    if (request.url === "/v1/models") return response.end(JSON.stringify({ data: [{ id: "skill-fixture" }] }));
    const chunks = []; for await (const chunk of request) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks).toString()); requests.push(body);
    await writeFile(resolve(root, "requests.json"), JSON.stringify(requests, null, 2));
    const results = body.messages.filter(m => m.role === "tool");
    const catalog = body.messages.filter(m => typeof m.content === "string" && m.content.includes("<available_skills>"));
    const instructions = body.messages.filter(m => typeof m.content === "string" && m.content.startsWith('<skill_content name="manual">'));
    assert.equal(body.tools.find(t => t.function.name === "skill").function.parameters.required[0], "name");
    assert.equal(instructions.length, 1, `human invocation missing; fixture ${root}`); assert.ok(instructions[0].content.includes("HUMAN INVOCATION BODY"));
    assert.ok(!catalog[0].content.includes("ignored")); assert.ok(!catalog[0].content.includes("Manual only"));
    let name = "example";
    if (results.length === 1) {
      assert.ok(results[0].content.includes("PROJECT BODY V1")); assert.ok(!results[0].content.includes("GLOBAL BODY"));
      await writeSkill(skillDir, "example", "Updated example description", "PROJECT BODY V2");
    } else if (results.length === 2) {
      assert.ok(results[1].content.includes("PROJECT BODY V2"));
      assert.equal(catalog.length, 2); assert.ok(catalog[1].content.includes("Updated example description"));
      const replacementIndex = body.messages.indexOf(catalog[1]); assert.ok(replacementIndex > body.messages.indexOf(results[1]));
      name = "manual";
    } else if (results.length === 3) {
      assert.equal(results[2].content, 'Error: skill "manual" is not available for model invocation');
      name = "missing";
      await unlink(resolve(skillDir, "example.md"));
      await unlink(resolve(globalHome, "skills/example.md"));
    } else if (results.length === 4) {
      assert.equal(results[3].content, 'Error: skill "missing" is unknown or no longer available');
      assert.ok(catalog.at(-1).content.includes("No skills are currently available"));
    }
    const message = results.length >= 4 ? { role: "assistant", content: "Native skill checks completed." }
      : { role: "assistant", tool_calls: [{ id: `skill.${results.length}`, type: "function", function: { name: "skill", arguments: JSON.stringify({ name }) } }] };
    response.setHeader("Content-Type", "text/event-stream");
    const delta = message.tool_calls ? { role: "assistant", tool_calls: message.tool_calls.map((c, index) => ({ index, ...c })) } : message;
    for (const chunk of [{ choices: [{ index: 0, delta, finish_reason: null }] }, { choices: [{ index: 0, delta: {}, finish_reason: message.tool_calls ? "tool_calls" : "stop" }] }, { choices: [], usage: { prompt_tokens: 20, completion_tokens: 10, total_tokens: 30 } }]) response.write(`data: ${JSON.stringify(chunk)}\n\n`);
    response.end("data: [DONE]\n\n");
  } catch (error) { failures.push(String(error)); response.statusCode = 500; response.end(String(error)); }
});
provider.listen(0, "127.0.0.1"); await once(provider, "listening");
const origin = `http://127.0.0.1:${provider.address().port}`; const port = 9264;
const executable = process.env.AWORKIT_QA_EXECUTABLE ?? resolve("src-tauri/target/debug/aworkit-desktop.exe");
const child = spawn(executable, [], { windowsHide: true, stdio: "ignore", env: { ...process.env, AWORKIT_QA_PROFILE: root, AWORKIT_QA_HIDE_WINDOW: "1", WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}` } });
let view;
const pause = () => new Promise(resolve => setTimeout(resolve, 100));
async function waitFor(expression) {
  for (let i = 0; i < 200; i++) { if (await view.evaluate(expression)) return; if (failures.length) throw new Error(failures.join("\n")); await pause(); }
  throw new Error(`Timed out: ${expression}\n${await view.evaluate("document.body.innerText")}`);
}
const click = name => waitFor(`(() => { const b=[...document.querySelectorAll('button')].find(b=>!b.disabled&&(b.title===${JSON.stringify(name)}||b.getAttribute('aria-label')===${JSON.stringify(name)}||b.textContent.trim().startsWith(${JSON.stringify(name)}))); if(!b)return false;b.click();return true;})()`);
async function setValue(selector, value) {
  await waitFor(`Boolean(document.querySelector(${JSON.stringify(selector + ":not(:disabled)")}))`);
  await view.evaluate(`(() => {const e=document.querySelector(${JSON.stringify(selector)});const p=e.tagName==='SELECT'?HTMLSelectElement.prototype:HTMLTextAreaElement.prototype;Object.getOwnPropertyDescriptor(p,'value').set.call(e,${JSON.stringify(value)});e.dispatchEvent(new Event(e.tagName==='SELECT'?'change':'input',{bubbles:true}));})()`);
}
try {
  for (let i = 0; i < 150 && !view; i++) { try { view = await connectNativeWebView(`http://127.0.0.1:${port}`); } catch { await pause(); } }
  assert.ok(view, "native WebView started");
  await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke)");
  await view.evaluate(`(async()=>{const invoke=window.__TAURI_INTERNALS__.invoke;
    const s=await invoke('settings_snapshot');await invoke('settings_commit',{command:{commandId:'skills.provider',expectedVersion:s.version,appearance:'system',portableHistoryEnabled:false,provider:{baseUrl:${JSON.stringify(origin + "/v1")},model:'skill-fixture',credentialAction:'keep',apiKey:null}}});
    const v=await invoke('settings_v2_snapshot');for(const p of v.settings.providers)for(const m of p.models)m.capabilities=['text','tools'];
    for(const tool of v.settings.tools)tool.enabled=true;
    const skill=v.settings.tools.find(t=>t.id==='tool.skill');skill.enabled=true;skill.configuration.aworkitHome=${JSON.stringify(globalHome)};skill.configuration.agentsHome=${JSON.stringify(resolve(root, "shared"))};
    v.settings.projects=[{id:'project.skills',name:'Skills fixture',workspace:{kind:'local_directory',location:${JSON.stringify(project)}},defaultWorkflowId:'workflow.simple-chat',portableHistoryEnabled:false}];
    await invoke('settings_v2_commit',{command:{commandId:'skills.tools',expectedVersion:v.version,settings:v.settings}});
    const w=await invoke('workflow_snapshot',{workflowId:'workflow.simple-chat'});w.document.nodes.find(n=>n.type==='agent').configuration.toolIds=[];
    await invoke('workflow_commit',{command:{commandId:'skills.workflow',expectedVersion:w.version,workflowId:'workflow.simple-chat',document:w.document}});})()`);
  await view.command("Page.reload");
  await click("Settings"); await click("Tools");
  await waitFor("Boolean(document.querySelector('[id=\"tool.skill-aworkitHome\"]'))");
  assert.ok(await view.evaluate("document.getElementById('tool.skill-aworkitHome').title.includes('~/.aworkit')"));
  await view.evaluate("document.getElementById('tool.skill-aworkitHome').closest('.settings-record').scrollIntoView({block:'start'})");
  await view.screenshot(resolve(root, "settings.png"));
  await click("Back to Chat");
  // Bind and validate in the actual editor, so frontend/native catalog drift fails here.
  await click("Workflows");
  await setValue('.workflow-library-bar select', "workflow.simple-chat");
  await waitFor("document.querySelector('.workflow-library-bar select')?.value === 'workflow.simple-chat' && document.body.innerText.includes('Version 2')");
  await view.evaluate("[...document.querySelectorAll('[aria-label=\"Workflow nodes\"] button')].find(b=>b.textContent==='Agent').click()");
  await waitFor("Boolean(document.querySelector('input[title=\"Bind Skills to this agent\"]'))");
  await view.evaluate("document.querySelector('input[title=\"Bind Skills to this agent\"]').click()");
  await click("Validate");
  await waitFor("document.body.innerText.includes('Validation passed: this workflow document is executable.')");
  assert.ok(!await view.evaluate("document.body.innerText.includes('no installed executor')"));
  await view.screenshot(resolve(root, "workflow-validation.png"));
  await click("Save");
  await waitFor("window.__TAURI_INTERNALS__.invoke('workflow_snapshot',{workflowId:'workflow.simple-chat'}).then(w=>w.document.nodes.find(n=>n.type==='agent').configuration.toolIds.includes('tool.skill'))");
  await click("Run");
  await setValue('select[aria-label="Workflow for the first Chat input"]', "workflow.simple-chat");
  await setValue('select[aria-label="Project for the first Chat input"]', "project.skills");
  await setValue('textarea[aria-label="Chat input"]', "Use /manual /manual and load example skills."); await click("Send");
  await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.phase==='waiting_input')");
  assert.deepEqual(failures, []); assert.equal(requests.length, 5);
  await view.screenshot(resolve(root, "chat.png"));
  await writeFile(resolve(root, "requests.json"), JSON.stringify(requests, null, 2));
  const report = { ok: true, root, requests: requests.length, cases: ["native Settings controls and tooltips", "workflow Skills selection, validation, save and Run", "project precedence", "ignored .dsh", "deduplicated human-only gesture", "lazy body reload", "append-only catalog replacement", "model invocation rejected", "unknown skill rejected", "empty catalog retirement"] };
  await writeFile(resolve(root, "result.json"), JSON.stringify(report, null, 2)); console.log(JSON.stringify(report));
} finally {
  view?.close(); if (child.exitCode === null) { const exited = once(child, "exit"); child.kill(); await exited; }
  provider.close();
}
