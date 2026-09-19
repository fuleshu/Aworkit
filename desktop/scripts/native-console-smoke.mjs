// Real Windows desktop -> MCP batch launcher, Shell and Python console probes.
// Isolated profile and local provider only; inspect OS handles, not just images.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { copyFile, mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve } from 'node:path';
import { connectNativeWebView } from './native-webview.mjs';

assert.equal(process.platform, 'win32');
const root = resolve(`src-tauri/target/native-console-${Date.now()}`);
await mkdir(root, { recursive: true });
const executable = resolve(root, 'aworkit-desktop.exe');
await copyFile(resolve(process.env.AWORKIT_QA_BINARY ?? 'src-tauri/target/debug/aworkit-desktop.exe'), executable);
const python = process.env.AWORKIT_QA_PYTHON ?? 'C:/Python313/python.exe';
const fixture = resolve('../crates/aworkit-capability-host/tests/fixtures/mcp_stdio_fixture.py');
const audit = resolve(root, 'startup.jsonl'), launcher = resolve(root, 'MCP launcher.cmd');
await writeFile(launcher, `@echo off\r\n"${python}" "${fixture}" --console-audit "${audit}"\r\n`);
const requests = [], checks = [], failures = [];
let child, view, logs = '', turn = 0;
function inspect(value) {
  if (typeof value === 'string') {
    let parsed; try { parsed = JSON.parse(value); } catch { return; }
    if (parsed !== value) inspect(parsed);
  }
  else if (value && typeof value === 'object') {
    if ('rootConsole' in value) {
      assert.equal(value.rootConsole, 0); assert.equal(value.childConsole, 0); checks.push(value);
    }
    for (const nested of Object.values(value)) inspect(nested);
  }
}
const quote = value => `'${value.replaceAll("'", "''")}'`;
const provider = createServer(async (req, res) => {
  try {
    if (req.method !== 'POST') return res.end(JSON.stringify({ data: [{ id: 'console-test' }] }));
    let raw = ''; for await (const chunk of req) raw += chunk;
    const body = JSON.parse(raw); requests.push(body); turn++;
    const result = body.messages.filter(m => m.role === 'tool').at(-1);
    if (result) { const before = checks.length; inspect(result.content); assert.ok(checks.length > before, result.content); }
    const echo = body.tools?.find(t => t.function.description === 'Return one message')?.function.name;
    assert.ok(echo, 'MCP tool is available to the Agent');
    const actions = [
      [echo, { message: '__console_probe__' }],
      ['shell', { command: `& ${quote(python)} ${quote(fixture)} --console-probe` }],
      ['python', { script: `import json, runpy\nf=runpy.run_path(${JSON.stringify(fixture)})\nprint(json.dumps(f['console_state']()))` }],
    ];
    const action = actions[turn - 1];
    const delta = action ? { tool_calls: [{ index: 0, id: `console.${turn}`, type: 'function', function: { name: action[0], arguments: JSON.stringify(action[1]) } }] } : { content: 'Console checks passed.' };
    res.setHeader('Content-Type', 'text/event-stream');
    res.end(`data: ${JSON.stringify({ choices: [{ index: 0, delta, finish_reason: null }] })}\n\n` +
      `data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: action ? 'tool_calls' : 'stop' }], usage: { prompt_tokens: 100, completion_tokens: 10 } })}\n\ndata: [DONE]\n\n`);
  } catch (error) { failures.push(String(error)); res.statusCode = 500; res.end(String(error)); }
});
provider.listen(0, '127.0.0.1'); await once(provider, 'listening');
const reservation = createServer(); reservation.listen(0, '127.0.0.1'); await once(reservation, 'listening');
const port = reservation.address().port; await new Promise(done => reservation.close(done));
const pause = () => new Promise(done => setTimeout(done, 100));
async function waitFor(expression) {
  for (let n = 0; n < 450; n++) { assert.deepEqual(failures, []); if (await view.evaluate(expression)) return; await pause(); }
  throw new Error(`Timeout: ${expression}\n${(await view.evaluate('document.body.innerText')).slice(-4000)}\n${logs}`);
}
async function click(label) {
  await waitFor(`(()=>{const b=[...document.querySelectorAll('button')].find(b=>!b.disabled&&b.getClientRects().length&&(b.title===${JSON.stringify(label)}||b.textContent.trim()===${JSON.stringify(label)}));if(!b)return false;b.click();return true;})()`);
}
async function setValue(selector, value) {
  await waitFor(`Boolean(document.querySelector(${JSON.stringify(selector)}))`);
  await view.evaluate(`(()=>{const e=document.querySelector(${JSON.stringify(selector)});Object.getOwnPropertyDescriptor(e.tagName==='SELECT'?HTMLSelectElement.prototype:HTMLTextAreaElement.prototype,'value').set.call(e,${JSON.stringify(value)});e.dispatchEvent(new Event(e.tagName==='SELECT'?'change':'input',{bubbles:true}));})()`);
}
try {
  child = spawn(executable, [], { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env,
    AWORKIT_QA_PROFILE: root, AWORKIT_QA_HIDE_WINDOW: '1', WEBVIEW2_USER_DATA_FOLDER: resolve(root, 'webview'),
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}` } });
  child.stdout.on('data', data => { logs += data; }); child.stderr.on('data', data => { logs += data; });
  for (let n = 0; n < 300; n++) { try { view = await connectNativeWebView(`http://127.0.0.1:${port}`); break; } catch { if (child.exitCode !== null) break; await pause(); } }
  assert.ok(view, logs); await waitFor('Boolean(window.__TAURI_INTERNALS__?.invoke)');
  await view.evaluate(`(async()=>{const i=window.__TAURI_INTERNALS__.invoke;
    const s=await i('settings_snapshot');await i('settings_commit',{command:{commandId:'console.provider',expectedVersion:s.version,appearance:'light',portableHistoryEnabled:false,provider:{baseUrl:${JSON.stringify(`http://127.0.0.1:${provider.address().port}/v1`)},model:'console-test',credentialAction:'keep',apiKey:null}}});
    const v=await i('settings_v2_snapshot');for(const p of v.settings.providers)for(const m of p.models)m.capabilities=['text','tools'];
    for(const t of v.settings.tools)if(['tool.shell.host','tool.python.host'].includes(t.id)){t.enabled=true;t.options={approvalMode:'full_access',executable:t.id==='tool.python.host'?${JSON.stringify(python)}:${JSON.stringify(resolve(process.env.SystemRoot, 'System32/WindowsPowerShell/v1.0/powershell.exe'))}};}
    const server={id:'mcp.console',name:'Console fixture',enabled:true,autoConnect:false,transport:{transport:'stdio',command:${JSON.stringify(launcher)},args:[],cwd:${JSON.stringify(root)},env:[]}};
    const probe=await i('settings_v2_probe_mcp',{request:{server,draftFingerprint:'console-fixture'}});if(!probe.tools?.length)throw new Error(JSON.stringify(probe));
    server.tools=probe.tools;v.settings.mcpServers=[server];await i('settings_v2_commit',{command:{commandId:'console.settings',expectedVersion:v.version,settings:v.settings}});
    const w=await i('workflow_snapshot',{workflowId:'workflow.simple-chat'});w.document.nodes.find(n=>n.type==='agent').configuration.toolIds=['tool.shell.host','tool.python.host','mcp:mcp.console'];
    await i('workflow_commit',{command:{commandId:'console.workflow',expectedVersion:w.version,workflowId:'workflow.simple-chat',document:w.document}});
    await i('workflow_set_default',{command:{commandId:'console.default',workflowId:'workflow.simple-chat'}});})()`);
  await view.command('Page.reload'); await click('Workflows'); await setValue('.workflow-library-bar select', 'workflow.simple-chat');
  const previous = await view.evaluate("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId)");
  await click('Run');
  await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId!==${JSON.stringify(previous)}&&document.body.innerText.includes(s.chat.runId)&&!document.querySelector('.workflow-library-bar')?.getClientRects().length)`);
  await setValue('select[aria-label="Workflow for the first Chat input"]', 'workflow.simple-chat');
  await setValue('textarea[aria-label="Chat input"]', 'Run the console probes in MCP, Shell and Python.');
  await waitFor("(()=>{const b=document.querySelector('.composer-input .primary-action:not(:disabled)');if(!b)return false;b.click();return true;})()");
  await waitFor("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.phase==='waiting_input'&&document.body.innerText.includes('Console checks passed.'))");
  assert.equal(requests.length, 4); assert.ok(checks.length >= 3);
  const starts = (await readFile(audit, 'utf8')).trim().split('\n').map(JSON.parse);
  assert.ok(starts.length >= 2); for (const start of starts) { assert.equal(start.rootConsole, 0); assert.equal(start.childConsole, 0); }
  await view.screenshot(resolve(root, 'verified.png'));
  console.log(JSON.stringify({ ok: true, root, toolChecks: checks.length, serverStarts: starts.length }));
} finally {
  view?.close();
  if (child?.exitCode === null) {
    const ended = once(child, 'exit'); const kill = spawn(resolve(process.env.SystemRoot, 'System32/taskkill.exe'), ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore' });
    await once(kill, 'exit'); await ended;
  }
  provider.close(); await writeFile(resolve(root, 'proof.json'), JSON.stringify({ requests, checks, failures, logs }, null, 2));
}
