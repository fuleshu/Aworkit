// Open a native Chat and verify the renderer stays responsive after hydration.
// AWORKIT_STARTUP_PROFILE explicitly opts into an existing profile; by default
// this uses its own profile and never edits or deletes another Chat's history.
import assert from 'node:assert/strict';
import { spawn, execFile } from 'node:child_process';
import { once } from 'node:events';
import { copyFile, mkdir, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { promisify } from 'node:util';
import { connectNativeWebView } from './native-webview.mjs';

const root = resolve(`src-tauri/target/native-startup-${Date.now()}`);
await mkdir(root, { recursive: true });
const executable = resolve(root, 'aworkit-desktop.exe');
await copyFile(resolve(process.env.AWORKIT_QA_BINARY ?? 'src-tauri/target/debug/aworkit-desktop.exe'), executable);
const profile = process.env.AWORKIT_STARTUP_PROFILE ?? root;
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
async function until(expression, label) {
  const deadline = performance.now() + 30000;
  while (performance.now() < deadline) {
    if (await view.evaluate(expression)) return;
    await pause(25);
  }
  throw Error(`Timed out waiting for ${label}`);
}
const started = performance.now();
let view, logs = '', result, monitoring = false;
const child = spawn(executable, [], { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'], env: {
  ...process.env, AWORKIT_QA_PROFILE: profile, AWORKIT_QA_HIDE_WINDOW: '1',
  WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: '--remote-debugging-port=9299',
} });
child.stdout.on('data', chunk => { logs += chunk; });
child.stderr.on('data', chunk => { logs += chunk; });
try {
  const deadline = performance.now() + 180_000;
  while (performance.now() < deadline) {
    assert.equal(child.exitCode, null, `App exited during startup: ${logs}`);
    if (!view) {
      try { view = await connectNativeWebView('http://127.0.0.1:9299'); } catch { await pause(200); continue; }
    }
    if (!monitoring) {
      await view.evaluate(`window.__frameGaps=[]; {let last=performance.now();const tick=()=>{const now=performance.now();window.__frameGaps.push(now-last);last=now;requestAnimationFrame(tick)};requestAnimationFrame(tick)}`);
      monitoring = true;
    }
    const state = await view.evaluate(`({ready:!!document.querySelector('textarea')&&!document.querySelector('.chat-main > .chat-busy'), failed:document.body.innerText.includes('Chat projection unavailable')})`);
    assert.equal(state.failed, false, 'Chat projection must be available');
    if (state.ready) break;
    await pause(250);
  }
  assert.ok(view && await view.evaluate("!!document.querySelector('textarea')"), `Chat did not open: ${logs}`);
  const readyMs = performance.now() - started;
  const snapshot = await view.evaluate(`window.__TAURI_INTERNALS__.invoke('desktop_chat_snapshot',{afterSequence:0}).then(s=>({chatId:s.chat.chatId,head:s.throughSequence,events:s.events.length,firstSequence:s.eventWindow.firstSequence,support:s.eventWindow.supportingEvents.length,historyCount:s.history.length,bytes:JSON.stringify(s).length}))`);
  assert.ok(snapshot.events <= 128, 'initial feed must be bounded');
  if (snapshot.head > 128) assert.ok(snapshot.firstSequence > 1, 'long Chat must open at the recent window');
  let olderRead = null, switching = null;
  await view.evaluate(`window.__feedReads=[];window.__feedRequests=[];window.__feedErrors=[]; window.__nativeFetch=window.fetch.bind(window); window.fetch=async(resource,options)=>{
    const command=typeof resource==='string'&&resource.startsWith('http://ipc.localhost/')?decodeURIComponent(new URL(resource).pathname.slice(1)):'';
    const args=command==='desktop_chat_snapshot'||command==='desktop_chat_events'?JSON.parse(options.body):{};
    if(command==='desktop_chat_snapshot' && args.chatId===window.__holdChat) { window.__heldRead=true; await new Promise(resolve=>window.__releaseRead=resolve); }
    if(command==='desktop_chat_events') window.__feedRequests.push(args);
    const response=await window.__nativeFetch(resource,options);
    if(command==='desktop_chat_events' && args.beforeSequence) { const result=await response.clone().json(); if(result.window) window.__feedReads.push({first:result.window.firstSequence,last:result.window.lastSequence,before:args.beforeSequence,events:result.events.length}); else window.__feedErrors.push(result); await new Promise(resolve=>setTimeout(resolve,250)); }
    return response;
  }`);
  const prepends = [];
  let beforeSequence = snapshot.firstSequence;
  for (let round = 0; round < 3 && beforeSequence > 1; round++) {
    const readStart = await view.evaluate('window.__feedReads.length');
    await view.evaluate(`{const scroll=document.querySelector('.timeline-scroll');scroll.dispatchEvent(new KeyboardEvent('keydown',{key:'Home',bubbles:true}));scroll.scrollTop=0;scroll.dispatchEvent(new Event('scroll'));}`);
    await until(`!!document.querySelector('.chat-history-loader .chat-busy')`, 'local older-history spinner');
    await view.evaluate(`{const scroll=document.querySelector('.timeline-scroll');const top=scroll.getBoundingClientRect().top;const row=[...scroll.querySelectorAll('[data-timeline-id]')].find(r=>r.getBoundingClientRect().bottom>top);window.__anchor=row?{id:row.dataset.timelineId,index:Number(row.dataset.index),model:!!row.querySelector('.model-call-block'),offset:row.getBoundingClientRect().top-top}:null;}`);
    await until(`window.__feedReads.length>${readStart}&&!document.querySelector('.chat-history-loader .chat-busy')`, 'older activity');
    await pause(200);
    olderRead = await view.evaluate(`({page:window.__feedReads[${readStart}],first:window.__feedReads.at(-1).first,pages:window.__feedReads.length-${readStart},anchor:window.__anchor,...(()=>{const scroll=document.querySelector('.timeline-scroll'),row=[...scroll.querySelectorAll('[data-timeline-id]')].find(r=>r.dataset.timelineId===window.__anchor?.id);return {offset:row?row.getBoundingClientRect().top-scroll.getBoundingClientRect().top:null,index:row?Number(row.dataset.index):null,top:scroll.scrollTop}})()})`);
    assert.equal(olderRead.page.last, beforeSequence-1, 'older page joins current window exactly');
    assert.ok(olderRead.page.first < beforeSequence, 'upward scrolling loads earlier activity');
    assert.ok(olderRead.index > olderRead.anchor.index, `Older activity must be prepended above the existing row: ${JSON.stringify(olderRead)}`);
    assert.ok(olderRead.offset !== null && Math.abs(olderRead.offset-olderRead.anchor.offset)<8, `Reading position moved: ${JSON.stringify(olderRead)}`);
    beforeSequence = olderRead.first;
    prepends.push(olderRead);
  }
  if (prepends.length) await view.screenshot(resolve(root, 'history-prepended.png'));
  const otherChats = await view.evaluate(`Array.from(document.querySelectorAll('[data-chat-id]')).map(e=>e.dataset.chatId).filter(id=>id!==${JSON.stringify(snapshot.chatId)}).slice(0,2)`);
  if (otherChats.length === 2) {
    await view.evaluate(`window.__holdChat=${JSON.stringify(otherChats[0])}; document.querySelector('[data-chat-id="'+window.__holdChat+'"] .chat-history-link').click()`);
    await until(`window.__heldRead===true`, 'held Chat read');
    const busy = await view.evaluate(`({selected:document.querySelector('[data-chat-id] button[aria-current="page"]')?.closest('[data-chat-id]').dataset.chatId,local:!!document.querySelector('.chat-main > .chat-busy')})`);
    assert.equal(busy.selected,otherChats[0]); assert.equal(busy.local,true);
    const clicked = performance.now();
    await view.evaluate(`document.querySelector('[data-chat-id="'+${JSON.stringify(otherChats[1])}+'"] .chat-history-link').click()`);
    await until(`document.querySelector('[data-chat-id="'+${JSON.stringify(otherChats[1])}+'"] button[aria-current="page"]')!==null`, 'immediate Chat selection');
    const selectionMs = performance.now()-clicked;
    await until(`!document.querySelector('.chat-main > .chat-busy')`, 'independent Chat load');
    switching = {selectionMs,readyMs:performance.now()-clicked};
    await view.evaluate(`window.__holdChat=null;window.__releaseRead()`);
    await pause(1000);
    assert.equal(await view.evaluate(`document.querySelector('[data-chat-id] button[aria-current="page"]')?.closest('[data-chat-id]').dataset.chatId`),otherChats[1], 'late response cannot replace selection');
    await view.evaluate(`document.querySelector('[data-chat-id="'+${JSON.stringify(snapshot.chatId)}+'"] .chat-history-link').click()`);
    await until(`!document.querySelector('.chat-main > .chat-busy') && document.querySelector('[data-chat-id="'+${JSON.stringify(snapshot.chatId)}+'"] button[aria-current="page"]')!==null`, 'original Chat restored');
  }
  let maxProbeMs = 0;
  for (let n = 0; n < 12; n++) {
    const before = performance.now();
    await view.evaluate('new Promise(resolve=>requestAnimationFrame(()=>resolve(true)))');
    maxProbeMs = Math.max(maxProbeMs, performance.now() - before);
    await pause(500);
  }
  assert.ok(maxProbeMs < 2000, `Renderer stalled during unchanged polling: ${maxProbeMs} ms`);
  await view.screenshot(resolve(root, 'startup.png'));
  const maxFrameGapMs = await view.evaluate('Math.max(...window.__frameGaps)');
  assert.ok(maxFrameGapMs < 1000, `Renderer froze during loading: ${maxFrameGapMs} ms`);
  result = { passed: true, readyMs, maxProbeMs, maxFrameGapMs, prepends, switching, ...snapshot };
  console.log(JSON.stringify(result));
} catch (error) {
  const diagnostic = await view?.evaluate(`({reads:window.__feedReads,requests:window.__feedRequests,errors:window.__feedErrors,busy:document.querySelector('.chat-history-loader')?.innerText,anchor:window.__anchor,top:document.querySelector('.timeline-scroll')?.scrollTop,frames:Math.max(...(window.__frameGaps??[0]))})`).catch(()=>null);
  await view?.screenshot(resolve(root, 'failure.png')).catch(()=>{});
  result = { passed: false, error: String(error), logs, diagnostic };
  console.log(JSON.stringify(result));
  throw error;
} finally {
  await writeFile(resolve(root, 'result.json'), JSON.stringify(result, null, 2));
  console.log(root);
  view?.close();
  if (child.exitCode === null) {
    const exited = once(child, 'exit');
    await promisify(execFile)('powershell.exe', ['-NoProfile', '-File', resolve('scripts/native-layout-window.ps1'), '-TargetProcessId', String(child.pid), '-Action', 'close'], { windowsHide: true });
    const timer = setTimeout(() => child.kill(), 15000);
    await exited;
    clearTimeout(timer);
  }
}
