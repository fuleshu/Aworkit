// Real native Chat: approval resume, final gate, stdin, retained jobs and soft yield.
// Pass --python to exercise both Python adapters using AWORKIT_PYTHON_EXECUTABLE.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { copyFile, mkdir, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve } from 'node:path';
import { connectNativeWebView } from './native-webview.mjs';

const python = process.argv.includes('--python');
const language = python ? 'python' : 'shell';
const pythonExecutable = process.env.AWORKIT_PYTHON_EXECUTABLE;
if (python) assert.ok(pythonExecutable, 'Set AWORKIT_PYTHON_EXECUTABLE to an absolute interpreter path');
const root = resolve(`src-tauri/target/native-${language}-jobs-${Date.now()}`);
await mkdir(root, { recursive: true });
const executable = resolve(root, 'aworkit-desktop.exe');
await copyFile(resolve(process.env.AWORKIT_QA_BINARY ?? 'src-tauri/target/debug/aworkit-desktop.exe'), executable);
const requests = [], failures = [];
let phase = 'interactive', turn = 0, interactiveJob, shellJob, readAttempts = 0;
const events = [];
let interactiveOutput = '';
const interactiveScript = "import ctypes, sys, time\nprint('console:' + str(ctypes.windll.kernel32.GetConsoleWindow()))\nprint('isolated:' + str(sys.flags.isolated))\nprint('received:' + input())\ntime.sleep(60)";
const descendantScript = "import subprocess, sys\nsubprocess.Popen([sys.executable, '-I', '-c', 'import time; time.sleep(60)'], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)\nprint('parent-exited')";
const provider = createServer(async (req, res) => {
  try {
    if (req.method !== 'POST') return res.end(JSON.stringify({ data: [{ id: 'shell-jobs' }] }));
    let raw = ''; for await (const chunk of req) raw += chunk;
    const body = JSON.parse(raw); requests.push(body); turn++;
    const toolMessages = body.messages.filter(m => m.role === 'tool');
    const last = toolMessages.length ? JSON.parse(toolMessages.at(-1).content) : null;
    const call = (name, args) => ({tool_calls:[{index:0,id:`jobs.${phase}.${turn}`,type:'function',function:{name,arguments:JSON.stringify(args)}}]});
    let message;
    if (phase === 'interactive') {
      if (turn === 1) message = python
        ? call('python_start', {script:interactiveScript, interactive:true})
        : call('shell_start', {command: 'powershell -NoProfile -NonInteractive -Command "$line=[Console]::ReadLine(); Write-Output (\'received:\' + $line); Start-Sleep -Seconds 60"', interactive:true});
      else if (turn === 2) {
        assert.equal(last.running, true, JSON.stringify(last)); interactiveJob = last.jobId;
        interactiveOutput += last.stdout ?? '';
        message = {content:'Premature final response: everything is done.'};
      } else if (turn === 3) {
        assert.ok(JSON.stringify(body).includes('Before finishing this response'), 'Runtime must reject the early final answer after approval resume');
        message = call('job_input', {jobId:interactiveJob,text:'hello-job\n',closeStdin:true});
      } else if (turn === 4) message = call('job_output', {jobId:interactiveJob,waitMs:1000});
      else if (last?.kept === true) message = {content:`Interactive retained job verified: ${interactiveJob}.`};
      else {
        interactiveOutput += last?.stdout ?? '';
        if (interactiveOutput.includes('received:hello-job')) {
          if (python) {
            assert.ok(interactiveOutput.includes('console:0'), interactiveOutput);
            assert.ok(interactiveOutput.includes('isolated:1'), interactiveOutput);
          }
          message = call('job_keep', {jobId:interactiveJob,reason:'Keep the fixture running to verify later Chat control.'});
        } else {
          assert.ok(++readAttempts < 10, JSON.stringify(last));
          message = call('job_output', {jobId:interactiveJob,waitMs:1000});
        }
      }
    } else if (phase === 'ordinary') {
      if (turn === 1) message = python ? call('python', {script:'import time; time.sleep(60)'}) : call('shell', {command:'powershell -NoProfile -NonInteractive -Command Start-Sleep -Seconds 60'});
      else if (turn === 2) { assert.equal(last.running,true,JSON.stringify(last)); shellJob=last.jobId; message=call('job_list',{}); }
      else if (turn === 3) {
        assert.equal(last.jobs.filter(j=>j.running).length,2,JSON.stringify(last));
        message=call('job_stop',{jobId:shellJob});
      } else if (turn === 4) {
        assert.equal(last.treeEmpty,true,JSON.stringify(last)); assert.equal(last.running,false);
        message=call('job_stop',{jobId:interactiveJob});
      } else { assert.equal(last.treeEmpty,true,JSON.stringify(last)); message={content:'Both process trees stopped and output collected.'}; }
    } else if (phase === 'descendant') {
      if (turn === 1) message = call('python_start', {script:descendantScript,interactive:false});
      else if (last?.status === 'stopped') {
        assert.equal(last.treeEmpty,true,JSON.stringify(last));
        message={content:'Python descendant stopped after its parent exited.'};
      } else if (last?.rootExited) {
        assert.equal(last.running,true,JSON.stringify(last));
        message=call('job_stop',{jobId:last.jobId});
      } else {
        assert.ok(turn<15,'Python parent should have exited');
        message=call('job_output',{jobId:last.jobId,waitMs:500});
      }
    } else if (phase === 'legacy') {
      if (turn === 1) message=call('python_start',{script:"raise RuntimeError('must never launch without controls')"});
      else if (turn === 2) {
        assert.ok(JSON.stringify(last).includes('Managed execution requires job_output'),JSON.stringify(last));
        message=call('python',{script:'import time; time.sleep(60)'});
      }
      else {
        assert.equal(last.error?.timedOut,true,JSON.stringify(last));
        assert.equal(last.jobId,undefined,JSON.stringify(last));
        message={content:'Legacy Python without controls retains its hard timeout.'};
      }
    }
    events.push({phase,turn,last,message});
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
  await waitFor(`(async()=>{
    const button=[...document.querySelectorAll('button')].find(b=>b.textContent==='Approve once'&&!b.disabled);
    if(button){button.click();return false;}
    const s=await window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0});
    return s.chat.phase==='waiting_input';
  })()`);
  assert.deepEqual(failures, []);
}
try {
  for (let i = 0; i < 300; i++) { try { view = await connectNativeWebView(`http://127.0.0.1:${port}`); break; } catch (error) { connectionError = String(error); if (child.exitCode !== null) break; await delay(); } }
  assert.ok(view, `Native WebView started (exit ${child.exitCode}, port ${port}): ${connectionError}\n${logs}`);
  await waitFor('Boolean(window.__TAURI_INTERNALS__?.invoke)');
  await view.evaluate(`(async()=>{const i=window.__TAURI_INTERNALS__.invoke;
    const s=await i('settings_snapshot');await i('settings_commit',{command:{commandId:'clock.provider',expectedVersion:s.version,appearance:'light',portableHistoryEnabled:false,provider:{baseUrl:${JSON.stringify(`http://127.0.0.1:${provider.address().port}/v1`)},model:'shell-jobs',credentialAction:'keep',apiKey:null}}});
    const v=await i('settings_v2_snapshot');for(const p of v.settings.providers)for(const m of p.models)m.capabilities=['text','tools'];
    for(const t of v.settings.tools)if(['tool.workspace_instructions','tool.${language}.host','tool.${language}.start','tool.job.input','tool.job.output','tool.job.list','tool.job.stop','tool.job.keep'].includes(t.id)){t.enabled=true;t.options={approvalMode:t.id==='tool.${language}.start'?'ask_for_approval':'full_access'};if(t.id==='tool.${language}.host')t.configuration.timeoutSeconds=1;if(${python}&&t.id.startsWith('tool.python.'))t.options.executable=${JSON.stringify(pythonExecutable ?? null)};if(t.id==='tool.workspace_instructions')t.configuration.aworkitHome=${JSON.stringify(resolve(root, 'global'))};}
    v.settings.projects=[{id:'project.clock',name:'Clock fixture',workspace:{kind:'local_directory',location:${JSON.stringify(root)}},defaultWorkflowId:'workflow.simple-chat',portableHistoryEnabled:false}];
    await i('settings_v2_commit',{command:{commandId:'clock.settings',expectedVersion:v.version,settings:v.settings}});
    const w=await i('workflow_snapshot',{workflowId:'workflow.simple-chat'});w.document.nodes.find(n=>n.type==='agent').configuration.toolIds=[];
    await i('workflow_commit',{command:{commandId:'clock.workflow',expectedVersion:w.version,workflowId:'workflow.simple-chat',document:w.document}});
    await i('workflow_set_default',{command:{commandId:'clock.default',workflowId:'workflow.simple-chat'}});})().catch(error=>{throw new Error(String(error))})`);
  await view.command('Page.reload'); await click('Workflows');
  await setValue('.workflow-library-bar select', 'workflow.simple-chat');
  for (const name of ['Workspace Instructions', python ? 'Host Python' : 'Host shell', python ? 'Start Python job' : 'Start shell job', 'Send job input', 'Read job output', 'List jobs', 'Stop job', 'Keep job running']) {
    await waitFor(`(()=>{const e=document.querySelector('input[title="Bind ${name} to this agent"]');if(e){if(!e.checked)e.click();return true;}[...document.querySelectorAll('[aria-label="Workflow nodes"] button')].find(b=>b.textContent==='Agent')?.click();return false;})()`);
  }
  await click('Validate'); await waitFor("document.body.innerText.includes('Validation passed: this workflow document is executable.')");
  await click('Save');
  const previousChat = await view.evaluate("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId)");
  await click('Run');
  await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId!==${JSON.stringify(previousChat)}&&document.body.innerText.includes(s.chat.runId)&&!document.querySelector('.workflow-library-bar')?.getClientRects().length)`);
  await setValue('select[aria-label="Workflow for the first Chat input"]', 'workflow.simple-chat');
  // Leave the project unselected: jobs must work in the Chat's private workspace.
  await setValue('select[aria-label="Project for the first Chat input"]', '');
  await send(`Start an interactive ${language} job, verify stdin, and intentionally keep it running.`);
  assert.ok(events.some(e=>e.turn===3&&e.phase==='interactive'));
  phase='ordinary'; turn=0;
  const started=Date.now();
  await send(`Run another long ${language} command, inspect both jobs, then stop both trees.`);
  assert.ok(Date.now()-started<25000,'ordinary process call must yield instead of blocking until exit');
  if (python) {
    phase='descendant'; turn=0;
    await send('Start Python with a child process, wait for the parent to exit, then stop the tree.');
    // A fresh Chat without the controls must retain the legacy hard timeout.
    await view.evaluate(`(async()=>{const i=window.__TAURI_INTERNALS__.invoke;
      const w=await i('workflow_snapshot',{workflowId:'workflow.simple-chat'});
      w.document.nodes.find(n=>n.type==='agent').configuration.toolIds=['tool.workspace_instructions','tool.python.host','tool.python.start'];
      await i('workflow_commit',{command:{commandId:'python.legacy-workflow',expectedVersion:w.version,workflowId:'workflow.simple-chat',document:w.document}});})()`);
    await click('Workflows');
    const previous = await view.evaluate("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId)");
    await click('Run');
    await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId!==${JSON.stringify(previous)})`);
    phase='legacy'; turn=0;
    await send('Run Python without job controls and verify its bounded legacy timeout.');
  }
  await writeFile(resolve(root, 'requests.json'), JSON.stringify({requests,events}, null, 2));
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
  await writeFile(resolve(root, 'requests.json'), JSON.stringify({requests,events,failures}, null, 2));
}
