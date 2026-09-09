// Real Tauri Chat controls, local provider fixture, native file tools and restart.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { copyFile, mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve } from 'node:path';
import { connectNativeWebView } from './native-webview.mjs';

const root = resolve(`src-tauri/target/native-projectless-${Date.now()}`);
await mkdir(root, { recursive: true });
const executable = resolve(root, 'aworkit-desktop.exe');
await copyFile(resolve(process.env.AWORKIT_QA_BINARY ?? 'src-tauri/target/debug/aworkit-desktop.exe'), executable);
const failures = [], requests = [], checks = [];
let scenario = 'standard', turn = 0;
const provider = createServer(async (req, res) => {
  try {
    if (req.method !== 'POST') return res.end(JSON.stringify({ data: [{ id: 'projectless' }] }));
    let raw = ''; for await (const part of req) raw += part;
    const body = JSON.parse(raw); requests.push({ scenario, body });
    const text = body.messages.map(m => m.content).filter(s => typeof s === 'string').join('\n');
    assert.ok(text.includes('Working folder for this Chat (no saved project selected)'), text.slice(0, 500));
    assert.ok(text.includes('chat-workspaces'));
    let message;
    const call = (name, args) => ({ tool_calls: [{ index: 0, id: `${scenario}.${turn}`, type: 'function', function: { name, arguments: JSON.stringify(args) } }] });
    if (text.includes('Return only one JSON object with exactly these fields:')) {
      message = { content: JSON.stringify({ goal: 'Use the Chat working folder', openQuestions: [], evidenceNeeded: [], toolOrder: ['aworkit_list_project_files'] }) };
    } else {
      turn++;
      if (scenario === 'standard' || scenario === 'isolated') {
        if (turn === 1) message = call('aworkit_list_project_files', { pattern: '**/*' });
        else {
          const output = JSON.parse(body.messages.filter(m => m.role === 'tool').at(-1).content);
          assert.ok(!JSON.stringify(output).includes('private-note.txt'), JSON.stringify(output));
          message = { content: `${scenario}: private folder listing succeeded.` };
        }
      } else if (scenario === 'write') {
        if (turn === 1) message = call('aworkit_write_project_file', { path: 'private-note.txt', content: 'persistent private content' });
        else message = { content: 'write: private file created.' };
      } else if (scenario === 'restart') {
        if (turn === 1) message = call('aworkit_read_project_file', { path: 'private-note.txt' });
        else {
          assert.ok(body.messages.filter(m => m.role === 'tool').at(-1).content.includes('persistent private content'));
          message = { content: 'restart: private file restored.' };
        }
      } else if (scenario === 'explicit') {
        assert.ok(text.includes('explicit.txt'), text);
        message = { content: 'explicit: file node succeeded.' };
      } else message = { content: 'simple: projectless chat succeeded.' };
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
async function send(label) {
  scenario = label; turn = 0;
  const before = await snapshot();
  await setValue('textarea[aria-label="Chat input"]', `Verify ${label} without a project.`);
  await click(before.chat.lockedWorkflow ? 'Queue' : 'Send');
  await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.version>${before.version}&&s.chat.phase==='waiting_input'&&s.events.some(e=>e.kind==='message.assistant'&&JSON.stringify(e.payload).includes(${JSON.stringify(label + ':')})))`);
  const after = await snapshot();
  assert.equal(after.chat.projectId, null);
  assert.equal(after.chat.scope, 'No project');
  assert.equal(after.projects.length, 0);
  assert.ok(!after.events.some(e => e.kind === 'span.failed'), JSON.stringify(after.events.filter(e => e.kind === 'span.failed')));
  checks.push({ scenario: label, chatId: after.chat.chatId, workflowId: after.chat.workflowId });
  console.log(`Verified ${label}`);
  return after;
}
async function newChat(workflowId) {
  const previous = await snapshot();
  await click('New Chat');
  await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId!==${JSON.stringify(previous.chat.chatId)}&&s.chat.phase==='draft')`);
  await setValue('select[aria-label="Workflow for the first Chat input"]', workflowId);
  assert.equal(await view.evaluate(`document.querySelector('select[aria-label="Project for the first Chat input"]').selectedOptions[0].textContent`), 'No project');
}
try {
  await launch();
  await view.evaluate(`(async()=>{const i=window.__TAURI_INTERNALS__.invoke;
    const s=await i('settings_snapshot');await i('settings_commit',{command:{commandId:'projectless.provider',expectedVersion:s.version,appearance:'light',portableHistoryEnabled:false,provider:{baseUrl:${JSON.stringify(`http://127.0.0.1:${provider.address().port}/v1`)},model:'projectless',credentialAction:'keep',apiKey:null}}});
    const v=await i('settings_v2_snapshot');for(const p of v.settings.providers)for(const m of p.models)m.capabilities=['text','tools'];
    for(const t of v.settings.tools)if(t.id!=='tool.mcp'){t.enabled=true;t.options={approvalMode:'full_access'};}
    v.settings.projects=[];await i('settings_v2_commit',{command:{commandId:'projectless.settings',expectedVersion:v.version,settings:v.settings}});})()`);
  await view.command('Page.reload');
  await newChat('workflow.standard-agent');
  await send('standard');
  await view.screenshot(resolve(root, 'standard-agent.png'));
  await newChat('workflow.simple-chat');
  await send('simple');

  // Save custom file tools, then exercise their normal first-send path.
  const custom = await invoke('workflow_snapshot', { workflowId: 'workflow.simple-chat' });
  custom.document.nodes.find(n => n.type === 'agent').configuration.toolIds = ['tool.files.write', 'tool.files.read'];
  await invoke('workflow_commit', { command: { commandId: 'projectless.file-workflow', expectedVersion: custom.version, workflowId: 'workflow.simple-chat', document: custom.document } });
  await newChat('workflow.simple-chat');
  const written = await send('write');
  const folder = resolve(root, 'runtime/chat-workspaces', written.chat.chatId);
  assert.equal(await readFile(resolve(folder, 'private-note.txt'), 'utf8'), 'persistent private content');
  await close(); await launch();
  assert.equal((await snapshot()).chat.chatId, written.chat.chatId);
  await send('restart');
  await newChat('workflow.standard-agent'); await send('isolated');

  const explicit = await invoke('workflow_snapshot', { workflowId: 'workflow.simple-chat' });
  explicit.document.nodes.find(n => n.type === 'agent').configuration.toolIds = [];
  explicit.document.nodes.push({ id: 'tool.private', type: 'tool', label: 'Write private file', position: { x: 130, y: 205 }, configuration: { toolId: 'tool.files.write', parameters: { path: 'explicit.txt', content: 'explicit-content' } } });
  explicit.document.edges.find(e => e.source === 'input.1').target = 'tool.private';
  explicit.document.edges.push({ id: 'private-agent', source: 'tool.private', target: 'agent.1' });
  await invoke('workflow_commit', { command: { commandId: 'projectless.explicit', expectedVersion: explicit.version, workflowId: 'workflow.simple-chat', document: explicit.document } });
  await newChat('workflow.simple-chat');
  const executed = await send('explicit');
  assert.equal(await readFile(resolve(root, 'runtime/chat-workspaces', executed.chat.chatId, 'explicit.txt'), 'utf8'), 'explicit-content');
  assert.deepEqual(failures, []);
  await view.screenshot(resolve(root, 'explicit-file-node.png'));
  await writeFile(resolve(root, 'evidence.json'), JSON.stringify({ checks, requests }, null, 2));
  console.log(JSON.stringify({ ok: true, checks, root }));
} finally { await close(); provider.close(); }
