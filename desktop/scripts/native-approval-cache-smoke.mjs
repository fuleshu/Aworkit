// Native upgrade, permission revocation, cache accounting and reviewer-prefix proof.
// Uses only an isolated QA profile and local HTTP fixture; no paid provider calls.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { copyFile, mkdir, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve } from 'node:path';
import { connectNativeWebView } from './native-webview.mjs';

const python = process.env.AWORKIT_PYTHON_EXECUTABLE;
assert.ok(python, 'Set AWORKIT_PYTHON_EXECUTABLE to an absolute Python executable');
const root = resolve(`src-tauri/target/native-approval-cache-${Date.now()}`);
await mkdir(root, { recursive: true });
const executable = resolve(root, 'aworkit-desktop.exe');
await copyFile(resolve(process.env.AWORKIT_QA_BINARY ?? 'src-tauri/target/debug/aworkit-desktop.exe'), executable);
const requests = [], failures = [], proof = {};
let phase = 'grant', turn = 0, child, view, logs = '';
const provider = createServer(async (req, res) => {
  try {
    if (req.method !== 'POST') return res.end(JSON.stringify({data:[{id:'approval-cache'}]}));
    let raw = ''; for await (const chunk of req) raw += chunk;
    const body = JSON.parse(raw);
    const review = body.messages[0]?.content.includes('independent approval reviewer');
    requests.push({phase, review, body});
    let delta;
    if (review) {
      assert.ok(Buffer.byteLength(raw) < 128 * 1024);
      assert.ok(body.messages.at(-1).content.includes('proposedAction'));
      delta = {content:JSON.stringify({decision:phase==='grant'?'ask_user':'approve',reason:'The fixture user requested these exact bounded commands.'})};
    } else {
      turn++;
      const results=body.messages.filter(m=>m.role==='tool');
      if (results.length) {
        const result=JSON.parse(results.at(-1).content);
        assert.ok(!result.error, JSON.stringify(result));
        assert.ok(JSON.stringify(result).includes('fixture-ok'), JSON.stringify(result));
      }
      const call=(name,args)=>({tool_calls:[{index:0,id:`${phase}.${turn}`,type:'function',function:{name,arguments:JSON.stringify(args)}}]});
      delta=turn===1?call('shell',{command:'echo fixture-ok'}):turn===2?call('python',{script:"print('fixture-ok')"}):{content:`Verified ${phase}.`};
    }
    const hit=review?60:80;
    res.setHeader('Content-Type','text/event-stream');
    res.end(`data: ${JSON.stringify({choices:[{index:0,delta,finish_reason:null}]})}\n\n`+
      `data: ${JSON.stringify({choices:[{index:0,delta:{},finish_reason:delta.tool_calls?'tool_calls':'stop'}],usage:{prompt_tokens:100,completion_tokens:5,prompt_cache_hit_tokens:hit,prompt_cache_miss_tokens:100-hit,prompt_tokens_details:{cached_tokens:hit}}})}\n\ndata: [DONE]\n\n`);
  } catch(error) { failures.push(String(error)); res.statusCode=500; res.end(String(error)); }
});
provider.listen(0,'127.0.0.1'); await once(provider,'listening');
const delay=()=>new Promise(r=>setTimeout(r,100));
async function waitFor(expression) {
  for(let n=0;n<450;n++) {
    assert.deepEqual(failures,[]);
    if(await view.evaluate(expression)) return;
    await delay();
  }
  throw new Error(`Timeout: ${expression}\n${(await view.evaluate('document.body.innerText')).slice(-6000)}\n${logs}`);
}
async function launch() {
  const reservation=createServer(); reservation.listen(0,'127.0.0.1'); await once(reservation,'listening');
  const port=reservation.address().port; await new Promise(r=>reservation.close(r));
  child=spawn(executable,[],{windowsHide:true,stdio:['ignore','pipe','pipe'],env:{...process.env,
    AWORKIT_QA_PROFILE:root,AWORKIT_QA_HIDE_WINDOW:'1',WEBVIEW2_USER_DATA_FOLDER:resolve(root,'webview'),
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:`--remote-debugging-port=${port}`}});
  child.stdout.on('data',v=>logs+=v); child.stderr.on('data',v=>logs+=v);
  for(let n=0;n<300;n++) { try {view=await connectNativeWebView(`http://127.0.0.1:${port}`);break;} catch {if(child.exitCode!==null)break;await delay();} }
  assert.ok(view,`Native WebView did not start: ${logs}`);
  await waitFor('Boolean(window.__TAURI_INTERNALS__?.invoke)');
}
async function stop() {
  view?.close(); view=undefined;
  if(child?.exitCode===null) {
    const end=once(child,'exit');
    const kill=spawn(resolve(process.env.SystemRoot,'System32/taskkill.exe'),['/PID',String(child.pid),'/T','/F'],{windowsHide:true,stdio:'ignore'});
    await once(kill,'exit'); await end;
  }
}
async function click(label) {
  await waitFor(`(()=>{const text=${JSON.stringify(label)};const b=[...document.querySelectorAll('button')].find(b=>!b.disabled&&b.getClientRects().length&&(b.title===text||b.textContent.trim()===text||b.getAttribute('aria-label')===text));if(!b)return false;b.click();return true;})()`);
}
async function setValue(selector,value) {
  await waitFor(`Boolean(document.querySelector(${JSON.stringify(selector)}))`);
  await view.evaluate(`(()=>{const e=document.querySelector(${JSON.stringify(selector)});Object.getOwnPropertyDescriptor(e.tagName==='SELECT'?HTMLSelectElement.prototype:HTMLTextAreaElement.prototype,'value').set.call(e,${JSON.stringify(value)});e.dispatchEvent(new Event(e.tagName==='SELECT'?'change':'input',{bubbles:true}));})()`);
}
async function newChat() {
  await click('Workflows');
  const previous=await view.evaluate("window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId)");
  await click('Run');
  await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(s=>s.chat.chatId!==${JSON.stringify(previous)}&&document.body.innerText.includes(s.chat.runId)&&!document.querySelector('.workflow-library-bar')?.getClientRects().length)`);
  await setValue('select[aria-label="Workflow for the first Chat input"]','workflow.simple-chat');
  await setValue('select[aria-label="Project for the first Chat input"]','project.cache');
}
async function send() {
  const text='Run echo fixture-ok in shell and print fixture-ok in Python, then report completion.';
  await waitFor(`(()=>{const e=document.querySelector('textarea[aria-label="Chat input"]');if(!e)return false;if(e.value!==${JSON.stringify(text)}){Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set.call(e,${JSON.stringify(text)});e.dispatchEvent(new Event('input',{bubbles:true}));return false;}const b=document.querySelector('.composer-input .primary-action:not(:disabled)');if(!b)return false;b.click();return true;})()`);
  await waitFor(`(async()=>{const approve=[...document.querySelectorAll('button')].find(b=>b.textContent==='Always approve in project'&&!b.disabled);if(approve&&${phase==='grant'}){approve.click();return false;}const s=await window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0});return s.chat.phase==='waiting_input'&&document.body.innerText.includes(${JSON.stringify(`Verified ${phase}.`)});})()`);
}
try {
  await launch();
  await view.evaluate(`(async()=>{const i=window.__TAURI_INTERNALS__.invoke;
    const s=await i('settings_snapshot');await i('settings_commit',{command:{commandId:'cache.provider',expectedVersion:s.version,appearance:'light',portableHistoryEnabled:false,provider:{baseUrl:${JSON.stringify(`http://127.0.0.1:${provider.address().port}/v1`)},model:'approval-cache',credentialAction:'keep',apiKey:null}}});
    const v=await i('settings_v2_snapshot');for(const p of v.settings.providers)for(const m of p.models)m.capabilities=['text','tools'];
    for(const t of v.settings.tools)if(['tool.shell.host','tool.python.host'].includes(t.id)){t.enabled=true;t.options={approvalMode:'approve_for_me'};if(t.id==='tool.python.host')t.options.executable=${JSON.stringify(python)};}
    v.settings.projects=[{id:'project.cache',name:'Cache fixture',workspace:{kind:'local_directory',location:${JSON.stringify(root)}},defaultWorkflowId:'workflow.simple-chat',portableHistoryEnabled:false}];
    await i('settings_v2_commit',{command:{commandId:'cache.settings',expectedVersion:v.version,settings:v.settings}});
    const w=await i('workflow_snapshot',{workflowId:'workflow.simple-chat'});w.document.nodes.find(n=>n.type==='agent').configuration.toolIds=['tool.shell.host','tool.python.host'];
    await i('workflow_commit',{command:{commandId:'cache.workflow',expectedVersion:w.version,workflowId:'workflow.simple-chat',document:w.document}});
    await i('workflow_set_default',{command:{commandId:'cache.default',workflowId:'workflow.simple-chat'}});})()`);
  await view.command('Page.reload'); await newChat(); await send();
  let grants=await view.evaluate("window.__TAURI_INTERNALS__.invoke('approval_project_grants')");
  assert.equal(grants.length,2); assert.ok(grants.every(g=>g.bindingHash.startsWith('execution-v1:')));
  await view.evaluate(`(async()=>{const i=window.__TAURI_INTERNALS__.invoke;const v=await i('settings_v2_snapshot');for(const t of v.settings.tools)if(['tool.shell.host','tool.python.host'].includes(t.id))t.options.instructions='Updated documentation for process tools';await i('settings_v2_commit',{command:{commandId:'cache.upgrade',expectedVersion:v.version,settings:v.settings}});})()`);
  await stop();
  // Reproduce the pre-upgrade grant format only in this fixture profile.
  const downgrade=spawn(python,['-c',`import hashlib,json,sqlite3,sys
from pathlib import Path
root=Path(sys.argv[1]).resolve()
assert root.parent==Path('src-tauri/target').resolve() and root.name.startswith('native-approval-cache-')
c=sqlite3.connect(root/'runtime/history/aworkit-invocations.sqlite3')
def digest(v): return hashlib.sha256(json.dumps(v,sort_keys=True,separators=(',',':'),ensure_ascii=False).encode()).hexdigest()
records=[json.loads(r[0])['record'] for r in c.execute("SELECT payload FROM semantic_events WHERE kind='pipeline.execution-prepared'")]
for id,body in list(c.execute('SELECT id,body FROM approval_project_grants')):
 g=json.loads(body)
 b=next(b for r in records if r['approvals']['projectKey']==g['projectKey'] for b in r['toolBindings'] if b['capabilityId']==g['capabilityId'])
 g['bindingHash']=digest(b);g['id']=digest([g['projectKey'],g['bindingHash'],g['actionHash']])
 c.execute('UPDATE approval_project_grants SET id=?,body=? WHERE id=?',(g['id'],json.dumps(g),id))
c.commit()
`,root],{windowsHide:true,stdio:['ignore','pipe','pipe']});
  let downgradeError='';downgrade.stderr.on('data',v=>downgradeError+=v);
  const [code]=await once(downgrade,'exit');assert.equal(code,0,downgradeError);
  phase='upgrade';turn=0;await launch();
  grants=await view.evaluate("window.__TAURI_INTERNALS__.invoke('approval_project_grants')");
  assert.equal(grants.length,2);assert.ok(grants.every(g=>g.bindingHash.startsWith('execution-v1:')));
  await newChat(); await send();
  assert.equal(requests.filter(r=>r.phase==='upgrade'&&r.review).length,0);
  proof.upgradeReviews=0;
  for(const grant of grants) await view.evaluate(`window.__TAURI_INTERNALS__.invoke('approval_revoke_project_grant',{id:${JSON.stringify(grant.id)}})`);
  phase='revoked';turn=0;await newChat();await send();
  const reviews=requests.filter(r=>r.phase==='revoked'&&r.review);
  assert.equal(reviews.length,2);proof.revokedReviews=2;
  assert.deepEqual(reviews[0].body.messages.slice(0,-2),reviews[1].body.messages.slice(0,-2));
  proof.reviewBytes=reviews.map(r=>Buffer.byteLength(JSON.stringify(r.body)));
  await waitFor("(()=>{if(document.querySelector('.run-details-inspector'))return true;const b=document.querySelector('button[title=\"Show or hide Run details\"]');if(b)b.click();return false;})()");
  await waitFor("(()=>{const b=document.querySelector('.run-details-breadcrumb button');if(!b)return false;b.click();return true;})()");
  await waitFor("document.querySelector('.run-details-inspector')?.innerText.includes('Cache hit rate')");
  const details=await view.evaluate("document.querySelector('.run-details-inspector').innerText");
  assert.ok(details.includes('80.0%'),details);assert.ok(details.includes('60.0%'),details);
  assert.ok(details.includes('Approval review usage'),details);
  proof.details=details;
  await view.screenshot(resolve(root,'verified.png'));
  console.log(JSON.stringify({ok:true,root,...proof}));
} finally {
  await stop();provider.close();
  await writeFile(resolve(root,'proof.json'),JSON.stringify({proof,requests,failures,logs},null,2));
}
