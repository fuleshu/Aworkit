// Real WM_CLOSE/relaunch and Win32 outer/client measurements, with an isolated profile.
import assert from 'node:assert/strict';
import { spawn, execFile } from 'node:child_process';
import { once } from 'node:events';
import { copyFile, mkdir, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { promisify } from 'node:util';
import { connectNativeWebView } from './native-webview.mjs';

const root = resolve(`src-tauri/target/native-layout-${Date.now()}`);
await mkdir(root, { recursive: true });
const executable = resolve(root, 'aworkit-desktop.exe');
await copyFile(resolve(process.env.AWORKIT_QA_BINARY ?? 'src-tauri/target/debug/aworkit-desktop.exe'), executable);
const pause = (ms = 100) => new Promise(resolve => setTimeout(resolve, ms));
let child, view, logs = '';
const evidence = [];
async function native(action, ...args) {
  const { stdout } = await promisify(execFile)('powershell.exe', ['-NoProfile', '-File', resolve('scripts/native-layout-window.ps1'), '-TargetProcessId', String(child.pid), '-Action', action, ...args], { windowsHide: true });
  return stdout.trim() ? JSON.parse(stdout) : null;
}
async function waitFor(expression) {
  for (let n = 0; n < 150; n++) {
    if (await view.evaluate(expression)) return;
    await pause();
  }
  throw new Error(`Timed out: ${expression}\n${logs}`);
}
async function launch() {
  child = spawn(executable, [], { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'], env: {
    ...process.env, AWORKIT_QA_PROFILE: root, AWORKIT_QA_HIDE_WINDOW: '1',
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: '--remote-debugging-port=9297',
  } });
  child.stdout.on('data', chunk => { logs += chunk; });
  child.stderr.on('data', chunk => { logs += chunk; });
  for (let n = 0; n < 150; n++) {
    try { view = await connectNativeWebView('http://127.0.0.1:9297'); break; } catch (error) { if (n === 149) logs += String(error); await pause(); }
  }
  assert.ok(view, `exit=${child.exitCode}, ${logs}`);
  await waitFor("Boolean(document.querySelector('.inspector-splitter'))");
  await pause(300);
}
async function close(action = 'close') {
  const exited = once(child, 'exit');
  await native(action);
  let timer;
  try {
    await Promise.race([exited, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('WM_CLOSE did not finish\n' + logs)), 12000); })]);
  } finally { clearTimeout(timer); }
  view.close(); view = undefined;
}
async function drag(selector, delta) {
  const at = await view.evaluate(`(() => {const r=document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect();return {x:r.left+r.width/2,y:r.top+r.height/2};})()`);
  await view.command('Input.dispatchMouseEvent', { type: 'mousePressed', ...at, button: 'left', clickCount: 1 });
  await view.command('Input.dispatchMouseEvent', { type: 'mouseMoved', x: at.x + delta, y: at.y, button: 'left', buttons: 1 });
  await pause(60);
  await view.command('Input.dispatchMouseEvent', { type: 'mouseReleased', x: at.x + delta, y: at.y, button: 'left', clickCount: 1 });
}
const panes = () => view.evaluate(`({historyPaneWidth:Number(document.querySelector('.desktop-shell > .pane-splitter').getAttribute('aria-valuenow')),inspectorPaneWidth:Number(document.querySelector('.inspector-splitter').getAttribute('aria-valuenow'))})`);
try {
  await launch();
  await native('place');
  await pause(200);
  await drag('.desktop-shell > .pane-splitter', 41.5);
  await drag('.inspector-splitter', -35.5);
  const before = { frame: await native('read'), panes: await panes() };
  evidence.push({ before });
  await close();
  for (let cycle = 1; cycle <= 3; cycle++) {
    await launch();
    const after = { frame: await native('read'), panes: await panes(), stored: await view.evaluate("window.__TAURI_INTERNALS__.invoke('desktop_layout')") };
    evidence.push({ cycle, after });
    for (const field of ['x', 'y', 'width', 'height']) assert.equal(after.frame[field], before.frame[field], `${field} survives restart ${cycle}`);
    for (const field of ['historyPaneWidth', 'inspectorPaneWidth']) assert.equal(Math.round(after.panes[field]), Math.round(before.panes[field]), `${field} survives restart ${cycle}`);
    await close(cycle === 2 ? 'quit' : 'close');
  }
  await launch();
  await native('maximize');
  await close();
  await launch();
  assert.equal((await native('read')).maximized, true, 'maximized state survives close');
  const maximizedLayout = await view.evaluate("window.__TAURI_INTERNALS__.invoke('desktop_layout')");
  evidence.push({ maximizedLayout });
  for (const field of ['x', 'y', 'width', 'height']) assert.equal(maximizedLayout[field], before.frame[field], `maximizing preserves normal ${field}`);
  await native('restore');
  const restored = await native('read');
  for (const field of ['x', 'y', 'width', 'height']) assert.equal(restored[field], before.frame[field], `unmaximize restores ${field}`);
  await native('minimize');
  await close();
  await launch();
  const afterMinimize = await native('read');
  evidence.push({ afterMinimize });
  assert.equal(afterMinimize.minimized, false, 'reopening never restores the minimized sentinel');
  for (const field of ['x', 'y', 'width', 'height']) assert.equal(afterMinimize[field], before.frame[field], `minimizing preserves normal ${field}`);
  await native('place', '-X', '-100', '-Y', '-9');
  await close('quit');
  await launch();
  const negativePosition = await native('read');
  evidence.push({ negativePosition });
  assert.equal(negativePosition.x, -100, 'reachable negative coordinates survive File > Quit');
  assert.equal(negativePosition.y, -9, 'the invisible top border does not invalidate a reachable title bar');
  await close();
  await writeFile(resolve(root, 'result.json'), JSON.stringify({ passed: true, evidence }, null, 2));
  console.log('PASS', root);
} catch (error) {
  await writeFile(resolve(root, 'failure.json'), JSON.stringify({ error: String(error), evidence, logs }, null, 2));
  console.error('FAIL', root);
  throw error;
} finally {
  view?.close();
  if (child?.exitCode === null) { const exited = once(child, 'exit'); child.kill(); await exited; }
}
