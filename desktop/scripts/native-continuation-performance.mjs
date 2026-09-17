// Optimized native runtime, real shell/Python, multi-gigabyte historical fixture.
// Build with profile.release.package.aworkit-desktop.debug-assertions=true to
// enable QA profile isolation. The code still uses release optimization.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { copyFile, mkdir, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve } from 'node:path';
import { connectNativeWebView } from './native-webview.mjs';

assert.ok(process.env.AWORKIT_QA_BINARY, 'An optimized executable with QA profile isolation is required');
const root = resolve(`src-tauri/target/native-continuation-${Date.now()}`);
await mkdir(root, { recursive: true });
const executable = resolve(root, 'aworkit-desktop.exe');
await copyFile(resolve(process.env.AWORKIT_QA_BINARY), executable);
const python = process.env.AWORKIT_QA_PYTHON ?? 'C:/Python313/python.exe';
const requests = [], failures = [];
let mode = 'warmup', nextTool = 0, warmupTool = false, previous, child, view, logs = '';
const actions = Array.from({ length: 8 }, (_, n) => n % 2
  ? ['python', { script: `print('PYTHON_CONTINUATION_${n}')` }]
  : ['shell', { command: `echo SHELL_CONTINUATION_${n}` }]);
const provider = createServer(async (req, res) => {
  try {
    if (req.method !== 'POST') return res.end(JSON.stringify({ data: [{ id: 'continuation', context_length: 1048576 }] }));
    let raw = ''; for await (const part of req) raw += part;
    const arrivedAt = Date.now(), body = JSON.parse(raw);
    const completedCall = body.messages.filter(m => m.role === 'tool').at(-1)?.tool_call_id;
    requests.push({ mode, arrivedAt, bytes: raw.length, completedCall });
    if (mode === 'measure' && previous) {
      assert.deepEqual(body.tools, previous.tools, 'stable tool definitions');
      assert.deepEqual(body.messages.slice(0, previous.messages.length), previous.messages, 'exact prompt prefix reuse');
    }
    if (mode === 'measure') previous = body;
    const action = mode === 'measure' ? actions[nextTool++] : warmupTool ? null : (warmupTool = true, ['shell', {command:'echo WARMUP'}]);
    const delta = action ? { tool_calls: [{ index: 0, id: `performance.call.${nextTool}`, type: 'function', function: {
      name: action[0], arguments: JSON.stringify(action[1]),
    } }] } : { content: `${mode} complete` };
    res.setHeader('Content-Type', 'text/event-stream');
    for (const value of [
      { choices: [{ index: 0, delta, finish_reason: null }] },
      { choices: [{ index: 0, delta: {}, finish_reason: action ? 'tool_calls' : 'stop' }] },
      { choices: [], usage: { prompt_tokens: mode === 'measure' ? 530000 : 500, completion_tokens: 20 } },
    ]) res.write(`data: ${JSON.stringify(value)}\n\n`);
    res.end('data: [DONE]\n\n');
  } catch (error) { failures.push(String(error)); res.writeHead(500); res.end(String(error)); }
});
provider.listen(0, '127.0.0.1'); await once(provider, 'listening');
const origin = `http://127.0.0.1:${provider.address().port}/v1`;
const reservation = createServer(); reservation.listen(0, '127.0.0.1'); await once(reservation, 'listening');
const port = reservation.address().port; await new Promise(done => reservation.close(done));
const pause = () => new Promise(done => setTimeout(done, 100));
async function waitFor(expression, seconds = 90) {
  const end = Date.now() + seconds * 1000;
  while (Date.now() < end) { if (await view.evaluate(expression)) return; await pause(); }
  throw new Error(`Timed out: ${expression}\n${await view.evaluate('document.body.innerText.slice(-4000)')}\n${logs.slice(-5000)}`);
}
async function start() {
  child = spawn(executable, [], { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'], env: {
    ...process.env, AWORKIT_QA_PROFILE: root, AWORKIT_QA_HIDE_WINDOW: '1',
    WEBVIEW2_USER_DATA_FOLDER: resolve(root, 'webview'), WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}`,
  } });
  child.stdout.on('data', x => { logs += x; }); child.stderr.on('data', x => { logs += x; });
  for (let n = 0; n < 600; n++) {
    try { view = await connectNativeWebView(`http://127.0.0.1:${port}`); await waitFor('Boolean(window.__TAURI_INTERNALS__?.invoke)'); return; }
    catch { if (child.exitCode !== null) throw new Error(logs); await pause(); }
  }
  throw new Error('Native startup timeout');
}
async function stop() {
  view?.close(); view = undefined;
  if (child?.exitCode === null) { const ended = once(child, 'exit'); child.kill(); await ended; }
}
async function command(action, payload) {
  return view.evaluate(`(async()=>{const s=await window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:Number.MAX_SAFE_INTEGER});return window.__TAURI_INTERNALS__.invoke('desktop_command',{command:{schemaVersion:1,commandId:${JSON.stringify(`performance.${Date.now()}`)},expectedVersion:s.chat.expectedVersion,action:${JSON.stringify(action)},targetId:s.chat.chatId,payload:${JSON.stringify(payload)}}});})()`);
}
async function send(text) {
  await view.evaluate(`(()=>{const e=document.querySelector('textarea[aria-label="Chat input"]');Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set.call(e,${JSON.stringify(text)});e.dispatchEvent(new Event('input',{bubbles:true}));})()`);
  await waitFor(`(()=>{const e=document.querySelector('.composer-submit:is([aria-label="Send"],[aria-label="Queue"]):not(:disabled)');if(!e)return false;e.click();return true;})()`);
  await waitFor(`document.querySelector('.timeline-scroll')?.textContent.includes(${JSON.stringify(`${mode} complete`)})`);
  await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:Number.MAX_SAFE_INTEGER}).then(s=>s.activeChatIds.length===0&&s.chat.phase==='waiting_input')`);
}
async function runPython(args) {
  const proc = spawn(python, args, { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  let output = ''; proc.stdout.on('data', c => { output += c; }); proc.stderr.on('data', c => { output += c; });
  const [code] = await once(proc, 'exit'); assert.equal(code, 0, output); return output;
}
try {
  await start();
  await view.evaluate(`(async()=>{const i=window.__TAURI_INTERNALS__.invoke;const s=await i('settings_snapshot');
    await i('settings_commit',{command:{commandId:'performance.provider',expectedVersion:s.version,appearance:'light',portableHistoryEnabled:false,provider:{baseUrl:${JSON.stringify(origin)},model:'continuation',credentialAction:'keep',apiKey:null}}});
    const v=await i('settings_v2_snapshot');for(const p of v.settings.providers)for(const m of p.models){m.capabilities=['text','tools'];m.contextWindow=1048576;}
    const ids=['tool.shell.host','tool.python.host','tool.workspace_instructions'];
    for(const t of v.settings.tools)if(ids.includes(t.id)){t.enabled=true;if(t.id==='tool.python.host')t.options={...t.options,executable:${JSON.stringify(python)}};}
    await i('settings_v2_commit',{command:{commandId:'performance.settings',expectedVersion:v.version,settings:v.settings}});
    const w=await i('workflow_snapshot',{workflowId:'workflow.simple-chat'});w.document.nodes.find(n=>n.type==='agent').configuration.toolIds=ids;
    await i('workflow_commit',{command:{commandId:'performance.workflow',expectedVersion:w.version,workflowId:'workflow.simple-chat',document:w.document}});
    await i('workflow_set_default',{command:{commandId:'performance.default',workflowId:'workflow.simple-chat'}});
  })()`);
  await view.command('Page.reload');
  await waitFor(`Boolean(document.querySelector('select[aria-label="Approval mode"]'))`);
  await view.evaluate(`(()=>{const e=document.querySelector('select[aria-label="Approval mode"]');Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype,'value').set.call(e,'full_access');e.dispatchEvent(new Event('change',{bubbles:true}));})()`);
  await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.approvalMode==='full_access')`);
  await send('Initialize the isolated continuation benchmark.');
  await stop();
  const fixture = JSON.parse(await runPython(['scripts/seed-continuation-history.py', root, process.env.AWORKIT_QA_SNAPSHOTS ?? '640']));
  console.log('Fixture:', fixture);
  mode = 'measure'; await start();
  await waitFor(`(()=>{const e=document.querySelector(${JSON.stringify(`[data-chat-id="${fixture.chatId}"] .chat-history-link`)});if(!e)return false;e.click();return true;})()`);
  await waitFor(`Boolean(document.querySelector('textarea[aria-label="Chat input"]:not(:disabled)'))`);
  await send('Continue the isolated benchmark with shell and Python.');
  await writeFile(resolve(root, 'requests.json'), JSON.stringify(requests, null, 2));
  const measurements = JSON.parse(await runPython(['scripts/measure-continuation-history.py', root]));
  await writeFile(resolve(root, 'result.json'), JSON.stringify({ fixture, measurements, failures }, null, 2));
  console.log(JSON.stringify({ root, fixture, measurements, failures }, null, 2));
  assert.deepEqual(failures, []);
  assert.equal(measurements.length, actions.length);
  assert.ok(measurements.every(m => m.delayMs >= 0 && m.delayMs < 1000), 'every tool continuation must dispatch within one second');
} finally {
  await writeFile(resolve(root, 'native.log'), logs); await stop();
  provider.closeAllConnections(); provider.close();
}
