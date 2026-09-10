// Native Chat UI -> trusted clock/dialect context -> actual cmd and PowerShell execution.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { copyFile, mkdir, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve } from 'node:path';
import { connectNativeWebView } from './native-webview.mjs';

const root = resolve(`src-tauri/target/native-clock-shell-${Date.now()}`);
await mkdir(root, { recursive: true });
const executable = resolve(root, 'aworkit-desktop.exe');
await copyFile(resolve(process.env.AWORKIT_QA_BINARY ?? 'src-tauri/target/debug/aworkit-desktop.exe'), executable);
const requests = [], failures = [];
let phase = 'date', turn = 0;
const provider = createServer(async (req, res) => {
  try {
    if (req.method !== 'POST') return res.end(JSON.stringify({ data: [{ id: 'clock-shell' }] }));
    let raw = ''; for await (const chunk of req) raw += chunk;
    const body = JSON.parse(raw); requests.push(body); turn++;
    const text = body.messages.map(m => m.content).filter(s => typeof s === 'string').join('\n');
    assert.ok(text.includes('Trusted Aworkit host clock for the latest user turn'));
    assert.ok(text.includes('no shell, Python or web verification'));
    assert.ok(text.includes('Windows cmd.exe syntax'));
    assert.ok(text.includes('date /t'));
    const clocks = [...text.matchAll(/Current turn UTC: (\S+)/g)];
    assert.ok(clocks.length);
    assert.ok(Math.abs(Date.now() - Date.parse(clocks.at(-1)[1])) < 120000);
    let message;
    if (phase === 'date') {
      assert.equal(turn, 1);
      message = { content: `Date supplied by trusted context: ${clocks.at(-1)[1].slice(0, 10)}.` };
    } else if (turn === 1) {
      const command = phase === 'cmd' ? 'date /t' : `powershell -NoProfile -NonInteractive -Command "Get-Date -Format 'yyyy-MM-dd'; Write-Output 'quoted text'"`;
      message = { tool_calls: [{ index: 0, id: `clock.${phase}`, type: 'function', function: { name: 'shell', arguments: JSON.stringify({ command }) } }] };
    } else {
      assert.equal(turn, 2);
      const result = JSON.parse(body.messages.filter(m => m.role === 'tool').at(-1).content);
      assert.equal(result.exitCode, 0, JSON.stringify(result));
      assert.ok(/\d/.test(result.stdout));
      if (phase === 'powershell') assert.ok(result.stdout.includes('quoted text'));
      message = { content: `${phase} clock command verified.` };
    }
    res.setHeader('Content-Type', 'text/event-stream');
    res.end(`data: ${JSON.stringify({ choices: [{ index: 0, delta: message, finish_reason: null }] })}\n\n` +
      `data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: message.tool_calls ? 'tool_calls' : 'stop' }], usage: { prompt_tokens: 1000, completion_tokens: 30 } })}\n\n` + 'data: [DONE]\n\n');
  } catch (error) { failures.push(String(error)); res.statusCode = 500; res.end(String(error)); }
});
provider.listen(0, '127.0.0.1'); await once(provider, 'listening');
const reservation = createServer(); reservation.listen(0, '127.0.0.1'); await once(reservation, 'listening');
const port = reservation.address().port; await new Promise(r => reservation.close(r));
const child = spawn(executable, [], { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env,
  AWORKIT_QA_PROFILE: root, AWORKIT_QA_HIDE_WINDOW: '1', WEBVIEW2_USER_DATA_FOLDER: resolve(root, 'webview'),
  WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}` } });
let view;
let logs = '';
let connectionError = '';
child.stdout.on('data', chunk => logs += chunk);
child.stderr.on('data', chunk => logs += chunk);
const delay = () => new Promise(r => setTimeout(r, 100));
async function waitFor(expression) {
  for (let i = 0; i < 300; i++) {
    if (failures.length) throw new Error(failures.join('\n'));
    if (await view.evaluate(expression)) return;
    await delay();
  }
  const controls = await view.evaluate("[...document.querySelectorAll('.composer-shell select,.composer-shell button')].map(e=>({label:e.getAttribute('aria-label'),value:e.value,disabled:e.disabled,title:e.title}))");
  throw new Error(`Timeout: ${expression}\n${JSON.stringify(controls)}\n${(await view.evaluate('document.body.innerText')).slice(-4000)}`);
}
async function setValue(selector, value) {
  await waitFor(`Boolean(document.querySelector(${JSON.stringify(selector)}))`);
  await view.evaluate(`(()=>{const e=document.querySelector(${JSON.stringify(selector)});Object.getOwnPropertyDescriptor(e.tagName==='SELECT'?HTMLSelectElement.prototype:HTMLTextAreaElement.prototype,'value').set.call(e,${JSON.stringify(value)});e.dispatchEvent(new Event(e.tagName==='SELECT'?'change':'input',{bubbles:true}));})()`);
}
async function click(text) {
  await waitFor(`(()=>{const n=${JSON.stringify(text)};const b=[...document.querySelectorAll('button')].find(b=>!b.disabled&&b.getClientRects().length&&(b.title===n||b.textContent.trim()===n||b.textContent.trim().startsWith(n+' ')||b.getAttribute('aria-label')===n));if(!b)return false;b.click();return true;})()`);
}
async function send(text) {
  await waitFor(`(()=>{const e=document.querySelector('textarea[aria-label="Chat input"]');if(!e)return false;if(e.value!==${JSON.stringify(text)}){Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set.call(e,${JSON.stringify(text)});e.dispatchEvent(new Event('input',{bubbles:true}));return false;}const b=document.querySelector('.composer-input .primary-action:not(:disabled)');if(!b)return false;b.click();return true;})()`);
  await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.phase==='waiting_input')");
  assert.deepEqual(failures, []);
}
try {
  for (let i = 0; i < 300; i++) { try { view = await connectNativeWebView(`http://127.0.0.1:${port}`); break; } catch (error) { connectionError = String(error); if (child.exitCode !== null) break; await delay(); } }
  assert.ok(view, `Native WebView started (exit ${child.exitCode}, port ${port}): ${connectionError}\n${logs}`);
  await waitFor('Boolean(window.__TAURI_INTERNALS__?.invoke)');
  await view.evaluate(`(async()=>{const i=window.__TAURI_INTERNALS__.invoke;
    const s=await i('settings_snapshot');await i('settings_commit',{command:{commandId:'clock.provider',expectedVersion:s.version,appearance:'light',portableHistoryEnabled:false,provider:{baseUrl:${JSON.stringify(`http://127.0.0.1:${provider.address().port}/v1`)},model:'clock-shell',credentialAction:'keep',apiKey:null}}});
    const v=await i('settings_v2_snapshot');for(const p of v.settings.providers)for(const m of p.models)m.capabilities=['text','tools'];
    for(const t of v.settings.tools)if(['tool.workspace_instructions','tool.shell.host'].includes(t.id)){t.enabled=true;t.options={approvalMode:'full_access'};if(t.id==='tool.workspace_instructions')t.configuration.aworkitHome=${JSON.stringify(resolve(root, 'global'))};}
    v.settings.projects=[{id:'project.clock',name:'Clock fixture',workspace:{kind:'local_directory',location:${JSON.stringify(root)}},defaultWorkflowId:'workflow.simple-chat',portableHistoryEnabled:false}];
    await i('settings_v2_commit',{command:{commandId:'clock.settings',expectedVersion:v.version,settings:v.settings}});
    const w=await i('workflow_snapshot',{workflowId:'workflow.simple-chat'});w.document.nodes.find(n=>n.type==='agent').configuration.toolIds=[];
    await i('workflow_commit',{command:{commandId:'clock.workflow',expectedVersion:w.version,workflowId:'workflow.simple-chat',document:w.document}});
    await i('workflow_set_default',{command:{commandId:'clock.default',workflowId:'workflow.simple-chat'}});})()`);
  await view.command('Page.reload'); await click('Workflows');
  await setValue('.workflow-library-bar select', 'workflow.simple-chat');
  for (const name of ['Workspace Instructions', 'Host shell']) {
    await waitFor(`(()=>{const e=document.querySelector('input[title="Bind ${name} to this agent"]');if(e){if(!e.checked)e.click();return true;}[...document.querySelectorAll('[aria-label="Workflow nodes"] button')].find(b=>b.textContent==='Agent')?.click();return false;})()`);
  }
  await click('Validate'); await waitFor("document.body.innerText.includes('Validation passed: this workflow document is executable.')");
  await click('Save');
  const previousChat = await view.evaluate("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId)");
  await click('Run');
  await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId!==${JSON.stringify(previousChat)}&&document.body.innerText.includes(s.chat.runId)&&!document.querySelector('.workflow-library-bar')?.getClientRects().length)`);
  await setValue('select[aria-label="Project for the first Chat input"]', 'project.clock');
  await setValue('select[aria-label="Workflow for the first Chat input"]', 'workflow.simple-chat');
  await send('What date is today?'); assert.equal(requests.length, 1);
  phase = 'cmd'; turn = 0; await send('Read the date with cmd date /t.');
  phase = 'powershell'; turn = 0; await send('Read the date with PowerShell.');
  await writeFile(resolve(root, 'requests.json'), JSON.stringify(requests, null, 2));
  await view.screenshot(resolve(root, 'verified.png'));
  console.log(JSON.stringify({ ok: true, requests: requests.length, root }));
} finally {
  view?.close();
  if (child.exitCode === null) {
    const end = once(child, 'exit');
    const killer = spawn(resolve(process.env.SystemRoot, 'System32/taskkill.exe'), ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore' });
    await once(killer, 'exit'); await end;
  }
  provider.close();
}
