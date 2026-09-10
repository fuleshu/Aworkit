// Real Tauri Chat controls, local provider fixture, native file tools and restart.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { copyFile, mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve } from 'node:path';
import { connectNativeWebView } from './native-webview.mjs';

const root = resolve(`src-tauri/target/native-file-access-${Date.now()}`);
await mkdir(root, { recursive: true });
const executable = resolve(root, 'aworkit-desktop.exe');
await copyFile(resolve(process.env.AWORKIT_QA_BINARY ?? 'src-tauri/target/debug/aworkit-desktop.exe'), executable);

const project = resolve(root, 'project'), external = resolve(root, 'external');
await mkdir(resolve(project, '.git'), {recursive:true});
await mkdir(external, {recursive:true});
await writeFile(resolve(project, '.git/HEAD'), 'ref: refs/heads/main\n');
await writeFile(resolve(project, 'inside.txt'), 'alpha');
await writeFile(resolve(external, 'outside.txt'), 'external alpha');
await writeFile(resolve(external, 'image.png'), Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC','base64'));
const failures = [], requests = [], checks = [];
let scenario = '', turn = 0, actions = [], reviewerDecision = 'approve';
const provider = createServer(async (req, res) => {
  try {
    if (req.method !== 'POST') return res.end(JSON.stringify({ data: [{ id: 'projectless' }] }));
    let raw = ''; for await (const part of req) raw += part;
    const body = JSON.parse(raw); requests.push({ scenario, body });
    const text = body.messages.map(m=>m.content).filter(s=>typeof s==='string').join('\n');
    let message;
    if (text.includes('independent approval reviewer')) {
      message = {content:JSON.stringify({decision:reviewerDecision,reason:'Fixture review of the external path.'})};
    } else {
      const names=(body.tools??[]).map(t=>t.function.name);
      assert.ok(names.every(n=>!n.startsWith('aworkit_')&&!n.includes('project')),JSON.stringify(names));
      const action=actions[turn++];
      message=action ? {tool_calls:[{index:0,id:scenario+'.'+turn,type:'function',function:{name:action[0],arguments:JSON.stringify(action[1])}}]}
        : {content:scenario+':done'};
    }
    const usage = { prompt_tokens: 1000, completion_tokens: 40, total_tokens: 1040 };
    if (body.stream) {
      res.setHeader('Content-Type', 'text/event-stream');
      res.end(`data: ${JSON.stringify({ choices: [{ index: 0, delta: message, finish_reason: null }] })}\n\n` +
        `data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: message.tool_calls ? 'tool_calls' : 'stop' }], usage })}\n\n` + 'data: [DONE]\n\n');
    } else {
      res.setHeader('Content-Type', 'application/json');
      res.end(JSON.stringify({ choices: [{ index: 0, message: { role: 'assistant', ...message }, finish_reason: 'stop' }], usage }));
    }
  } catch (error) { failures.push(String(error)); res.statusCode = 500; res.end(String(error)); }
});
provider.listen(0, '127.0.0.1'); await once(provider, 'listening');
const reservation = createServer(); reservation.listen(0, '127.0.0.1'); await once(reservation, 'listening');
const port = reservation.address().port; await new Promise(r => reservation.close(r));
console.log(JSON.stringify({ root, port }));
let child, view, logs = '';
const pause = () => new Promise(r => setTimeout(r, 100));
const invoke = (command, args = {}) => view.evaluate(`window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)},${JSON.stringify(args)})`);
const snapshot = () => invoke('desktop_snapshot', { afterSequence: 0 });
async function launch() {
  child = spawn(executable, [], { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env,
    AWORKIT_QA_PROFILE: root, AWORKIT_QA_HIDE_WINDOW: '1', WEBVIEW2_USER_DATA_FOLDER: resolve(root, 'webview'),
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}` } });
  child.stdout.on('data', data => logs += data); child.stderr.on('data', data => logs += data);
  for (let i = 0; i < 300; i++) {
    try { view = await connectNativeWebView(`http://127.0.0.1:${port}`); break; } catch { if (child.exitCode !== null) break; await pause(); }
  }
  assert.ok(view, `Native WebView did not start: ${logs}`);
  await waitFor('Boolean(window.__TAURI_INTERNALS__?.invoke)');
}
async function close() {
  view?.close(); view = undefined;
  if (child?.exitCode === null) {
    const ended = once(child, 'exit');
    const killer = spawn(resolve(process.env.SystemRoot, 'System32/taskkill.exe'), ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore' });
    await once(killer, 'exit'); await ended;
  }
}
async function waitFor(expression) {
  for (let i = 0; i < 300; i++) {
    if (failures.length) throw new Error(failures.join('\n'));
    if (await view.evaluate(expression)) return;
    await pause();
  }
  throw new Error(`Timed out: ${expression}\n${(await view.evaluate('document.body.innerText')).slice(-4500)}\n${logs}`);
}
async function click(name) {
  await waitFor(`(()=>{const name=${JSON.stringify(name)};const e=[...document.querySelectorAll('button')].find(e=>!e.disabled&&e.getClientRects().length&&(e.title===name||e.textContent.trim().replace(/^[＋]\\s*/,'')===name||e.getAttribute('aria-label')===name));if(!e)return false;e.click();return true;})()`);
}
async function setValue(selector, value) {
  await waitFor(`Boolean(document.querySelector(${JSON.stringify(selector + ':not(:disabled)')}))`);
  await view.evaluate(`(()=>{const e=document.querySelector(${JSON.stringify(selector)});Object.getOwnPropertyDescriptor(e.tagName==='SELECT'?HTMLSelectElement.prototype:HTMLTextAreaElement.prototype,'value').set.call(e,${JSON.stringify(value)});e.dispatchEvent(new Event(e.tagName==='SELECT'?'change':'input',{bubbles:true}));})()`);
}

async function start(label, calls, mode='ask_for_approval', selectedProject='project.files') {
  scenario=label; actions=calls; turn=0;
  const before=await snapshot();
  await click('New Chat');
  await waitFor('window.__TAURI_INTERNALS__.invoke("desktop_snapshot",{afterSequence:0}).then(s=>s.chat.chatId!=='+JSON.stringify(before.chat.chatId)+'&&s.chat.phase==="draft")');
  await setValue('select[aria-label="Approval mode"]',mode);
  await waitFor('window.__TAURI_INTERNALS__.invoke("desktop_snapshot",{afterSequence:0}).then(s=>s.chat.approvalMode==='+JSON.stringify(mode)+')');
  await setValue('select[aria-label="Workflow for the first Chat input"]','workflow.simple-chat');
  await setValue('select[aria-label="Project for the first Chat input"]',selectedProject);
  await setValue('textarea[aria-label="Chat input"]','Verify '+label+' file operations.');
  await click('Send');
}
async function completed() {
  await waitFor('window.__TAURI_INTERNALS__.invoke("desktop_snapshot",{afterSequence:0}).then(s=>s.chat.phase==="waiting_input"&&s.events.some(e=>e.kind==="message.assistant"&&JSON.stringify(e.payload).includes('+JSON.stringify(scenario+':done')+')))');
  const s=await snapshot();
  assert.ok(!s.events.some(e=>e.kind==='span.failed'&&e.payload.status!=='denied'),JSON.stringify(s.events.filter(e=>e.kind==='span.failed')));
  checks.push({scenario,chatId:s.chat.chatId});
  console.log('Verified '+scenario);
}
async function waiting() {
  await waitFor('window.__TAURI_INTERNALS__.invoke("desktop_snapshot",{afterSequence:0}).then(s=>s.chat.phase==="awaiting_approval")');
  await view.evaluate('document.querySelector(".timeline-scroll")?.scrollTo(0,1e9)');
  await waitFor('[...document.querySelectorAll("button")].some(e=>!e.disabled&&e.textContent==="Approve once")');
  assert.ok(!await view.evaluate('[...document.querySelectorAll("button")].some(e=>e.getClientRects().length&&!e.disabled&&e.textContent==="Always approve in project")'));
}
try {
  await launch();
  await view.evaluate('(async()=>{const i=window.__TAURI_INTERNALS__.invoke;const s=await i("settings_snapshot");await i("settings_commit",{command:{commandId:"files.provider",expectedVersion:s.version,appearance:"light",portableHistoryEnabled:false,provider:{baseUrl:'+JSON.stringify('http://127.0.0.1:'+provider.address().port+'/v1')+',model:"files",credentialAction:"keep",apiKey:null}}});const v=await i("settings_v2_snapshot");for(const p of v.settings.providers)for(const m of p.models)m.capabilities=["text","tools","vision"];for(const t of v.settings.tools){t.enabled=true;t.options={};}v.settings.projects=[{id:"project.files",name:"File access fixture",workspace:{kind:"local_directory",location:'+JSON.stringify(project)+'},defaultWorkflowId:"workflow.simple-chat",portableHistoryEnabled:false}];await i("settings_v2_commit",{command:{commandId:"files.settings",expectedVersion:v.version,settings:v.settings}});const w=await i("workflow_snapshot",{workflowId:"workflow.simple-chat"});w.document.nodes.find(n=>n.type==="agent").configuration.toolIds=v.settings.tools.filter(t=>t.id.startsWith("tool.files.")||t.id==="tool.image.read").map(t=>t.id);await i("workflow_commit",{command:{commandId:"files.workflow",expectedVersion:w.version,workflowId:"workflow.simple-chat",document:w.document}});})()');
  await view.command('Page.reload');
  await click('Settings'); await click('Open Tools: Built-in tool availability and bindings');
  await waitFor('document.body.innerText.includes("File read")');
  await view.screenshot(resolve(root,'file-tool-settings.png'));
  await click('Back to Chat');
  await start('inside',[
    ['read_file',{path:'inside.txt'}],
    ['write_file',{path:resolve(project,'created.txt'),content:'created'}],
    ['edit_file',{path:'created.txt',old_string:'created',new_string:'changed'}],
    ['search_file',{path:resolve(project,'created.txt'),query:'changed'}],
    ['list_files',{path:project,pattern:'*.txt'}],
    ['grep_files',{path:project,pattern:'changed'}],
  ]);
  await completed();
  assert.equal(await readFile(resolve(project,'created.txt'),'utf8'),'changed');
  assert.ok(!(await snapshot()).events.some(e=>e.kind==='approval.reviewed'));
  await view.screenshot(resolve(root,'inside-no-approval.png'));

  await start('outside-read-restart',[['read_file',{path:resolve(external,'outside.txt')}]]);
  await waiting();
  await view.screenshot(resolve(root,'external-approval.png'));
  assert.ok(!requests.at(-1).body.messages.some(m=>m.role==='tool'&&String(m.content).includes('external alpha')));
  const count=requests.length;
  await close(); await launch(); await waiting();
  assert.equal(requests.length,count,'restart does not repeat the model request');
  await click('Approve once'); await completed();
  assert.ok(requests.at(-1).body.messages.some(m=>m.role==='tool'&&String(m.content).includes('external alpha')));

  await start('outside-denied',[['write_file',{path:resolve(external,'outside.txt'),content:'denied write'}]]);
  await waiting(); await click('Deny and give reason');
  await setValue('textarea[aria-label="Reason for denial"]','Keep the external file unchanged.');
  await click('Deny action'); await completed();
  assert.equal(await readFile(resolve(external,'outside.txt'),'utf8'),'external alpha');

  for (const action of [
    ['search_file',{path:resolve(external,'outside.txt'),query:'alpha'}],
    ['list_files',{path:external,pattern:'*.txt'}],
    ['grep_files',{path:external,pattern:'alpha'}],
    ['edit_file',{path:resolve(external,'outside.txt'),old_string:'alpha',new_string:'beta'}],
    ['read_image',{path:resolve(external,'image.png')}],
  ]) {
    await start('outside-'+action[0],[action]);
    await waiting(); await click('Approve once'); await completed();
  }
  assert.equal(await readFile(resolve(external,'outside.txt'),'utf8'),'external beta');
  await start('automatic',[['write_file',{path:resolve(external,'outside.txt'),content:'automatic'}]],'approve_for_me');
  await completed();
  assert.equal(await readFile(resolve(external,'outside.txt'),'utf8'),'automatic');
  assert.ok((await snapshot()).events.some(e=>e.kind==='approval.reviewed'));

  const reviews=requests.filter(r=>r.body.messages.some(m=>String(m.content).includes('independent approval reviewer'))).length;
  await start('full-access',[['write_file',{path:resolve(external,'outside.txt'),content:'full'}]],'full_access');
  await completed();
  assert.equal(await readFile(resolve(external,'outside.txt'),'utf8'),'full');
  assert.equal(requests.filter(r=>r.body.messages.some(m=>String(m.content).includes('independent approval reviewer'))).length,reviews);

  await start('projectless-external',[['read_file',{path:resolve(external,'outside.txt')}]],'ask_for_approval','');
  await waiting(); await click('Approve once'); await completed();
  assert.equal((await snapshot()).chat.projectId,null);
  assert.deepEqual(await invoke('approval_project_grants'),[]);
  assert.deepEqual(failures,[]);
  await writeFile(resolve(root,'evidence.json'),JSON.stringify({ok:true,checks,requests},null,2));
  console.log(JSON.stringify({ok:true,root,checks}));
} finally { await close(); provider.close(); }
