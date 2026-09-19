// Real WebView/composer, local provider, isolated profile. No paid API calls.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { copyFile, mkdir, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve } from 'node:path';
import { connectNativeWebView } from './native-webview.mjs';

const root = resolve(`src-tauri/target/native-agent-prefix-${Date.now()}`);
await mkdir(root, { recursive: true });
const executable = resolve(root, 'aworkit-desktop.exe');
await copyFile(resolve(process.env.AWORKIT_QA_BINARY ?? 'src-tauri/target/debug/aworkit-desktop.exe'), executable);
const python = process.env.AWORKIT_PYTHON_EXECUTABLE ?? 'C:/Python313/python.exe';
const requests = [], failures = [], proof = { prefixComparisons: 0, approvalRestart: false };
let child, view, logs = '', phase = 'tools', turn = 0, mainCalls = 0, previous;
const planText = () => `PLAN_${phase}_${turn}: ${'bounded generated planning context '.repeat(180)}`;
const provider = createServer(async (req, res) => {
  try {
    if (req.method !== 'POST') return res.end(JSON.stringify({ data: [{ id: 'prefix-test' }] }));
    let raw = ''; for await (const part of req) raw += part;
    const body = JSON.parse(raw), planning = body.messages[0]?.content.includes('PREFIX_FIXTURE_PLANNER');
    requests.push({ phase, turn, planning, body });
    let delta;
    if (planning) {
      delta = { content: planText() };
    } else {
      mainCalls++;
      assert.ok(!body.messages[0].content.includes('PLAN_'), 'Changing plans must not enter system prefix');
      for (let n = 1; n <= turn; n++) {
        const marker = `PLAN_${phase}_${n}:`;
        assert.equal(body.messages.filter(m => typeof m.content === 'string' && m.content.includes(marker)).length, 1, `${marker} is admitted once`);
      }
      const planIndex = body.messages.findIndex(m => m.content?.includes(`PLAN_${phase}_${turn}:`));
      const userIndex = body.messages.findIndex(m => m.content === `User request ${phase} ${turn}`);
      assert.ok(userIndex >= 0 && planIndex > userIndex, 'Current graph context follows current user');
      assert.equal(body.messages[planIndex].role, 'user');
      if (previous) {
        assert.deepEqual(body.tools, previous.tools, 'Frozen tool definitions');
        assert.deepEqual(body.messages.slice(0, previous.messages.length), previous.messages, 'Exact previous request prefix');
        proof.prefixComparisons++;
      }
      previous = body;
      const result = body.messages.filter(m => m.role === 'tool').at(-1);
      if (result) assert.ok(result.content.includes('prefix-ok') && !JSON.parse(result.content).error, result.content);
      if (phase === 'tools' && mainCalls <= 2) {
        const name = mainCalls === 1 ? 'shell' : 'python';
        const args = name === 'shell' ? { command: 'echo prefix-ok' } : { script: "print('prefix-ok')" };
        delta = { tool_calls: [{ index: 0, id: `${phase}.${turn}.${mainCalls}`, type: 'function', function: { name, arguments: JSON.stringify(args) } }] };
      } else delta = { content: `Completed ${phase} ${turn}.` };
    }
    res.setHeader('Content-Type', 'text/event-stream');
    res.end(`data: ${JSON.stringify({ choices: [{ index: 0, delta, finish_reason: null }] })}\n\n` +
      `data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: delta.tool_calls ? 'tool_calls' : 'stop' }], usage: { prompt_tokens: 4000, completion_tokens: 20 } })}\n\ndata: [DONE]\n\n`);
  } catch (error) { failures.push(String(error)); res.statusCode = 500; res.end(String(error)); }
});
provider.listen(0, '127.0.0.1'); await once(provider, 'listening');
const pause = () => new Promise(done => setTimeout(done, 100));
async function waitFor(expression) {
  for (let n = 0; n < 450; n++) {
    assert.deepEqual(failures, []);
    if (await view.evaluate(expression)) return;
    await pause();
  }
  throw new Error(`Timeout: ${expression}\n${(await view.evaluate('document.body.innerText')).slice(-5000)}\n${logs}`);
}
async function launch() {
  const reservation = createServer(); reservation.listen(0, '127.0.0.1'); await once(reservation, 'listening');
  const port = reservation.address().port; await new Promise(done => reservation.close(done));
  child = spawn(executable, [], { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'], env: {
    ...process.env, AWORKIT_QA_PROFILE: root, AWORKIT_QA_HIDE_WINDOW: '1',
    WEBVIEW2_USER_DATA_FOLDER: resolve(root, 'webview'), WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}`,
  } });
  child.stdout.on('data', data => { logs += data; }); child.stderr.on('data', data => { logs += data; });
  for (let n = 0; n < 300; n++) {
    try { view = await connectNativeWebView(`http://127.0.0.1:${port}`); break; }
    catch { if (child.exitCode !== null) break; await pause(); }
  }
  assert.ok(view, `Native startup: ${logs}`); await waitFor('Boolean(window.__TAURI_INTERNALS__?.invoke)');
}
async function stop() {
  view?.close(); view = undefined;
  if (child?.exitCode === null) {
    const ended = once(child, 'exit');
    const kill = spawn(resolve(process.env.SystemRoot, 'System32/taskkill.exe'), ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore' });
    await once(kill, 'exit'); await ended;
  }
}
async function click(label) {
  await waitFor(`(()=>{const b=[...document.querySelectorAll('button')].find(b=>!b.disabled&&b.getClientRects().length&&(b.title===${JSON.stringify(label)}||b.textContent.trim()===${JSON.stringify(label)}));if(!b)return false;b.click();return true;})()`);
}
async function setValue(selector, value) {
  await waitFor(`Boolean(document.querySelector(${JSON.stringify(selector)}))`);
  await view.evaluate(`(()=>{const e=document.querySelector(${JSON.stringify(selector)});Object.getOwnPropertyDescriptor(e.tagName==='SELECT'?HTMLSelectElement.prototype:HTMLTextAreaElement.prototype,'value').set.call(e,${JSON.stringify(value)});e.dispatchEvent(new Event(e.tagName==='SELECT'?'change':'input',{bubbles:true}));})()`);
}
async function newChat() {
  await click('Workflows'); await setValue('.workflow-library-bar select', 'workflow.simple-chat');
  const old = await view.evaluate("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId)");
  await click('Run');
  await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId!==${JSON.stringify(old)}&&!document.querySelector('.workflow-library-bar')?.getClientRects().length)`);
  await setValue('select[aria-label="Workflow for the first Chat input"]', 'workflow.simple-chat');
}
async function send(restartAtApproval = false) {
  turn++; mainCalls = 0;
  await setValue('textarea[aria-label="Chat input"]', `User request ${phase} ${turn}`);
  await waitFor("(()=>{const b=document.querySelector('.composer-input .primary-action:not(:disabled)');if(!b)return false;b.click();return true;})()");
  if (restartAtApproval) {
    await waitFor("[...document.querySelectorAll('button')].some(b=>b.textContent==='Approve once'&&!b.disabled)");
    await stop(); await launch(); proof.approvalRestart = true;
  }
  await waitFor(`(async()=>{const b=[...document.querySelectorAll('button')].find(b=>b.textContent==='Approve once'&&!b.disabled);if(b){b.click();return false;}const s=await window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0});return s.chat.phase==='waiting_input'&&document.body.innerText.includes(${JSON.stringify(`Completed ${phase} ${turn}.`)});})()`);
}
try {
  await launch();
  await view.evaluate(`(async()=>{const i=window.__TAURI_INTERNALS__.invoke;
    const s=await i('settings_snapshot');await i('settings_commit',{command:{commandId:'prefix.provider',expectedVersion:s.version,appearance:'light',portableHistoryEnabled:false,provider:{baseUrl:${JSON.stringify(`http://127.0.0.1:${provider.address().port}/v1`)},model:'prefix-test',credentialAction:'keep',apiKey:null}}});
    const v=await i('settings_v2_snapshot');for(const p of v.settings.providers)for(const m of p.models)m.capabilities=['text','tools'];
    for(const t of v.settings.tools)if(['tool.shell.host','tool.python.host'].includes(t.id)){t.enabled=true;t.options={approvalMode:'ask_for_approval'};if(t.id==='tool.python.host')t.options.executable=${JSON.stringify(python)};}
    await i('settings_v2_commit',{command:{commandId:'prefix.settings',expectedVersion:v.version,settings:v.settings}});
    const w=await i('workflow_snapshot',{workflowId:'workflow.simple-chat'});const a=w.document.nodes.find(n=>n.type==='agent');a.configuration.toolIds=['tool.shell.host','tool.python.host'];
    a.configuration.instructions='PREFIX_FIXTURE_AGENT: consume the latest graph context.';
    const e=w.document.edges.find(e=>e.target===a.id);e.target='prefix.planner';
    w.document.nodes.push({id:'prefix.planner',type:'model_call',label:'Planner',position:{x:150,y:100},configuration:{modelTierId:'tier:balanced',instructions:'PREFIX_FIXTURE_PLANNER: produce the current plan.'}});
    w.document.edges.push({id:'prefix.planner-agent',source:'prefix.planner',target:a.id});
    await i('workflow_commit',{command:{commandId:'prefix.workflow',expectedVersion:w.version,workflowId:'workflow.simple-chat',document:w.document}});
    await i('workflow_set_default',{command:{commandId:'prefix.default',workflowId:'workflow.simple-chat'}});})()`);
  await view.command('Page.reload'); await newChat(); await send(true); await send();
  await stop(); await launch(); await send();
  assert.equal(requests.filter(r => r.planning).length, 3, 'Resume must not rerun planning');
  assert.equal(requests.filter(r => !r.planning).length, 9, 'Exactly two tools and one answer per user turn');
  await view.evaluate(`(async()=>{const i=window.__TAURI_INTERNALS__.invoke;const w=await i('workflow_snapshot',{workflowId:'workflow.simple-chat'});w.document.nodes.find(n=>n.type==='agent').configuration.toolIds=[];await i('workflow_commit',{command:{commandId:'prefix.text',expectedVersion:w.version,workflowId:'workflow.simple-chat',document:w.document}});})()`);
  phase = 'text'; turn = 0; previous = undefined;
  await newChat(); await send(); await stop(); await launch(); await send();
  assert.equal(proof.prefixComparisons, 9);
  assert.equal(requests.length, 16);
  await view.screenshot(resolve(root, 'verified.png'));
  console.log(JSON.stringify({ ok: true, root, ...proof }));
} finally {
  await stop(); provider.close();
  await writeFile(resolve(root, 'proof.json'), JSON.stringify({ proof, requests, failures, logs }, null, 2));
}
