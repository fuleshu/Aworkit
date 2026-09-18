// Real Standard Agent workflow: Stop, steer, restart, then a normal follow-up.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { copyFile, mkdir, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve } from 'node:path';
import { connectNativeWebView } from './native-webview.mjs';

const root = resolve(`src-tauri/target/native-steering-${Date.now()}`);
await mkdir(root, { recursive: true });
const executable = resolve(root, 'aworkit-desktop.exe');
await copyFile(resolve(process.env.AWORKIT_QA_BINARY ?? 'src-tauri/target/debug/aworkit-desktop.exe'), executable);
const requests = [], failures = [], holds = new Map();
let mode = 'initial', toolRequested = false, sleepingToolRequested = false;
const provider = createServer(async (req, res) => {
  try {
    if (req.method !== 'POST') return res.end(JSON.stringify({ data: [{ id: 'steering-fixture', context_length: 32768 }] }));
    let raw = ''; for await (const part of req) raw += part;
    const body = JSON.parse(raw);
    const planner = body.messages.some(m => m.role === 'system' && String(m.content).includes('Return only one JSON object with exactly these fields:'));
    requests.push({ mode, planner, body });
    res.setHeader('Content-Type', 'text/event-stream');
    const chunk = delta => res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta, finish_reason: null }] })}\n\n`);
    const finish = delta => {
      chunk(delta);
      res.end(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: delta.tool_calls ? 'tool_calls' : 'stop' }], usage: { prompt_tokens: 900, completion_tokens: 40 } })}\n\ndata: [DONE]\n\n`);
    };
    if (planner && mode !== 'stop-plan') return finish({ content: JSON.stringify({ goal: 'Preserve this original plan', openQuestions: [], evidenceNeeded: [], toolOrder: ['shell'] }) });
    if (!planner && mode === 'initial' && !toolRequested) {
      toolRequested = true;
      return finish({ tool_calls: [{ index: 0, id: 'steering.tool.once', type: 'function', function: { name: 'shell', arguments: JSON.stringify({ command: 'echo STEERING_TOOL_EVIDENCE' }) } }] });
    }
    if (!planner && mode === 'tool-stop' && !sleepingToolRequested) {
      sleepingToolRequested = true;
      return finish({ tool_calls: [
        { index: 0, id: 'steering.tool.stopped', type: 'function', function: { name: 'shell', arguments: JSON.stringify({ command: 'powershell -NoProfile -Command "Start-Sleep -Seconds 3"' }) } },
        { index: 1, id: 'steering.tool.unstarted', type: 'function', function: { name: 'shell', arguments: JSON.stringify({ command: 'echo SHOULD_NOT_RUN' }) } },
      ] });
    }
    if (['initial', 'steer-one', 'stop-plan'].includes(mode)) {
      chunk({ content: planner ? '{"goal":"unfinished' : 'Still working. ' });
      const timer = setInterval(() => res.write(': heartbeat\n\n'), 100);
      holds.set(mode, () => { clearInterval(timer); res.end(); });
      res.on('close', () => clearInterval(timer));
      return;
    }
    finish({ content: `Finished ${mode}` });
  } catch (error) { failures.push(String(error)); res.end(); }
});
provider.listen(0, '127.0.0.1'); await once(provider, 'listening');
const origin = `http://127.0.0.1:${provider.address().port}/v1`;
const reservation = createServer(); reservation.listen(0, '127.0.0.1'); await once(reservation, 'listening');
const port = reservation.address().port; await new Promise(done => reservation.close(done));
let child, view, logs = '';
const delay = () => new Promise(done => setTimeout(done, 100));
const invoke = (command, args = {}) => view.evaluate(`window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)},${JSON.stringify(args)})`);
const snapshot = () => invoke('desktop_snapshot', { afterSequence: 0 });
async function launch() {
  child = spawn(executable, [], { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env,
    AWORKIT_QA_PROFILE: root, AWORKIT_QA_HIDE_WINDOW: '1', WEBVIEW2_USER_DATA_FOLDER: resolve(root, 'webview'),
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}` } });
  child.stdout.on('data', data => logs += data); child.stderr.on('data', data => logs += data);
  for (let i = 0; i < 200; i++) {
    try { view = await connectNativeWebView(`http://127.0.0.1:${port}`); break; } catch { await delay(); }
  }
  assert.ok(view, logs);
  await waitFor('Boolean(window.__TAURI_INTERNALS__?.invoke)');
}
async function close() {
  view?.close(); view = undefined;
  if (child?.exitCode === null) { const ended = once(child, 'exit'); child.kill(); await ended; }
}
async function waitFor(expression) {
  for (let i = 0; i < 200; i++) {
    assert.deepEqual(failures, []);
    if (await view.evaluate(expression)) return;
    await delay();
  }
  throw new Error(`Timed out: ${expression}\n${await view.evaluate('document.body.innerText')}\n${logs}`);
}
async function click(selector) {
  await waitFor(`Boolean(document.querySelector(${JSON.stringify(selector + ':not(:disabled)')}))`);
  await view.evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`);
}
async function send(nextMode) {
  mode = nextMode;
  await waitFor(`Boolean(document.querySelector('textarea[aria-label="Chat input"]:not(:disabled)'))`);
  await view.evaluate(`(() => {const e=document.querySelector('textarea[aria-label="Chat input"]');Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set.call(e,${JSON.stringify(nextMode)});e.dispatchEvent(new Event('input',{bubbles:true}));})()`);
  await click('.composer-submit:is([aria-label="Send"],[aria-label="Queue"])');
  if (mode === 'tool-stop') {
    await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.events.some(e=>e.kind==='span.started'&&e.payload.callId==='steering.tool.stopped'))");
  } else if (['initial', 'steer-one', 'stop-plan'].includes(mode)) {
    for (let i = 0; i < 200 && !holds.has(mode); i++) { assert.deepEqual(failures, []); await delay(); }
    assert.ok(holds.has(mode), `provider held ${mode}`);
  } else {
    await waitFor(`document.querySelector('.timeline-scroll')?.textContent.includes(${JSON.stringify('Finished ' + mode)})`);
    await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.activeChatIds.length===0)");
  }
  await waitFor(`document.querySelector('textarea[aria-label="Chat input"]')?.value === ''`);
}
async function stop(nodeId) {
  const before = (await snapshot()).events.filter(e => e.kind === 'chat.turn_stopped').length;
  await click('button[aria-label="Stop response"]');
  await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.activeChatIds.length===0&&s.events.filter(e=>e.kind==='chat.turn_stopped').length>${before})`);
  const stopped = (await snapshot()).events.filter(e => e.kind === 'chat.turn_stopped').at(-1);
  assert.equal(stopped.payload.nodeId, nodeId);
  assert.ok(stopped.payload.continuationRequestId);
  holds.get(mode)?.();
}
async function newChat() {
  const id = (await snapshot()).chat.chatId;
  await click('button.new-chat');
  await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId!==${JSON.stringify(id)})`);
  await waitFor('Boolean(document.querySelector(".composer-submit[aria-label=Send]"))');
}
try {
  await launch();
  await view.evaluate(`(async()=>{const invoke=window.__TAURI_INTERNALS__.invoke;const s=await invoke('settings_snapshot');
    await invoke('settings_commit',{command:{commandId:'steering.provider',expectedVersion:s.version,appearance:'light',portableHistoryEnabled:false,provider:{baseUrl:${JSON.stringify(origin)},model:'steering-fixture',credentialAction:'keep',apiKey:null}}});
    const v=await invoke('settings_v2_snapshot');for(const p of v.settings.providers)for(const m of p.models){m.capabilities=['text','tools'];m.contextWindow=32768;}
    v.settings.tools.find(t=>t.id==='tool.shell.host').enabled=true;
    await invoke('settings_v2_commit',{command:{commandId:'steering.models',expectedVersion:v.version,settings:v.settings}});
    const w=await invoke('workflow_snapshot',{workflowId:'workflow.standard-agent'});w.document.nodes.find(n=>n.type==='agent').configuration.toolIds=['tool.shell.host'];
    await invoke('workflow_commit',{command:{commandId:'steering.workflow',expectedVersion:w.version,workflowId:'workflow.standard-agent',document:w.document}});
    await invoke('workflow_set_default',{command:{commandId:'steering.default',workflowId:'workflow.standard-agent'}});
  })()`);
  await view.command('Page.reload');
  await waitFor('Boolean(document.querySelector("textarea"))');
  const draft = await snapshot();
  await invoke('desktop_command', { command: { schemaVersion: 1, commandId: 'steering.approvals', expectedVersion: draft.version,
    action: 'approval_mode', targetId: draft.chat.chatId, payload: { mode: 'full_access' } } });
  await view.command('Page.reload');
  await send('initial');
  const chat = (await snapshot()).chat;
  await stop('agent.1');
  await send('steer-one');
  assert.equal(requests.filter(r => r.planner).length, 1, 'steering does not rerun the planner');
  const steered = requests.at(-1).body;
  assert.ok(steered.messages.some(m => m.role === 'tool' && m.content.includes('STEERING_TOOL_EVIDENCE')));
  assert.equal(steered.messages.filter(m => m.role === 'user' && m.content === 'steer-one').length, 1);
  await stop('agent.1');
  await close(); await launch();
  await send('steer-two');
  assert.equal(requests.filter(r => r.planner).length, 1, 'restart and a second Stop retain the Agent position');
  assert.equal((await snapshot()).chat.runId, chat.runId);
  await send('fresh-followup');
  assert.equal(requests.filter(r => r.planner).length, 2, 'completed response follow-up starts at the planner');
  assert.equal((await snapshot()).events.filter(e => e.kind === 'tool.requested' && e.payload.callId === 'steering.tool.once').length, 1, 'the settled tool was never replayed');
  await newChat();
  await send('stop-plan'); await stop('plan.1');
  await send('steer-plan');
  const plannerSteer = requests.find(r => r.mode === 'steer-plan' && r.planner);
  assert.ok(plannerSteer, 'an interrupted planner is itself steered');
  assert.equal(plannerSteer.body.messages.filter(m => m.role === 'user' && m.content === 'steer-plan').length, 1);
  await newChat();
  await send('tool-stop'); await stop('agent.1');
  await send('steer-tool');
  const toolSteer = requests.find(r => r.mode === 'steer-tool' && !r.planner);
  assert.ok(!requests.some(r => r.mode === 'steer-tool' && r.planner), 'tool cancellation resumes its Agent');
  assert.ok(toolSteer.body.messages.some(m => m.role === 'tool' && m.tool_call_id === 'steering.tool.stopped'), 'the interrupted tool outcome is retained in model context');
  assert.ok(toolSteer.body.messages.some(m => m.role === 'tool' && m.tool_call_id === 'steering.tool.unstarted' && m.content.includes('not_executed_after_stop')), 'unstarted batch calls are identified without being dispatched');
  assert.ok(!(await snapshot()).events.some(e => e.kind === 'span.started' && e.payload.callId === 'steering.tool.unstarted'), 'the authority never dispatched the remaining call');
  await view.screenshot(resolve(root, 'steered-chat.png'));
  await writeFile(resolve(root, 'result.json'), JSON.stringify({ passed: true, providerCalls: requests.length,
    checks: ['Agent steering', 'preserved tool results', 'no tool replay', 'repeated Stop', 'restart', 'fresh follow-up replans', 'planner steering', 'in-flight tool steering'] }, null, 2));
  console.log(JSON.stringify({ passed: true, root, providerCalls: requests.length }));
} catch (error) {
  if (view) {
    await writeFile(resolve(root, 'failed-ui.txt'), await view.evaluate('document.body.innerText'));
    await writeFile(resolve(root, 'failed-snapshot.json'), JSON.stringify(await snapshot(), null, 2));
    await view.screenshot(resolve(root, 'failed.png'));
  }
  console.error('Native artifacts:', root); throw error;
} finally {
  for (const release of holds.values()) release();
  await writeFile(resolve(root, 'requests.json'), JSON.stringify(requests, null, 2));
  await writeFile(resolve(root, 'native.log'), logs);
  await close(); provider.closeAllConnections(); provider.close();
}
