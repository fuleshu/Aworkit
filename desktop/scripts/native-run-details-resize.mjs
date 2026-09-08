// Isolated native WebView2 rendering benchmark with a completed-run projection.
// Large diagnostic records are injected at the snapshot boundary, never into user history.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { copyFile, mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { connectNativeWebView } from "./native-webview.mjs";

const root = resolve(`src-tauri/target/native-run-details-resize-${Date.now()}`);
await mkdir(root, { recursive: true });
const executable = resolve(root, "aworkit-desktop.exe");
await copyFile(resolve(process.env.AWORKIT_QA_BINARY ?? "src-tauri/target/debug/aworkit-desktop.exe"), executable);
const pause = (ms = 100) => new Promise(resolve => setTimeout(resolve, ms));
let child, view, logs = "";
let stage = "startup";
async function waitFor(expression) {
  for (let n = 0; n < 150; n++) { if (await view.evaluate(expression)) return; await pause(); }
  throw new Error(`Timed out: ${expression}\n` + await view.evaluate("document.body.innerText"));
}
async function point(selector) {
  return view.evaluate(`(() => {const r=document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect();return {x:r.left+r.width/2,y:r.top+r.height/2};})()`);
}
async function click(selector) {
  const at = await point(selector);
  for (const type of ["mousePressed", "mouseReleased"]) await view.command("Input.dispatchMouseEvent", { type, ...at, button: "left", clickCount: 1 });
}
async function metrics() {
  const result = await view.command("Performance.getMetrics");
  return Object.fromEntries(result.metrics.map(({ name, value }) => [name, value]));
}
try {
  child = spawn(executable, [], { windowsHide: true, stdio: ["ignore", "pipe", "pipe"], env: {
    ...process.env, AWORKIT_QA_PROFILE: root, AWORKIT_QA_HIDE_WINDOW: "1",
    WEBVIEW2_USER_DATA_FOLDER: resolve(root, "webview"),
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: "--remote-debugging-port=9287",
  } });
  child.stdout.on("data", chunk => { logs += chunk; }); child.stderr.on("data", chunk => { logs += chunk; });
  for (let n = 0; n < 150; n++) { try { view = await connectNativeWebView("http://127.0.0.1:9287"); break; } catch (error) { if(n===149)logs+=String(error); await pause(); } }
  assert.ok(view, `Native process exit ${child.exitCode}: ${logs}`);
  await view.command("Emulation.setDeviceMetricsOverride", { width: 1600, height: 1000, deviceScaleFactor: 1, mobile: false });
  await waitFor("Boolean(document.querySelector('.inspector-tabs'))");
  stage = "inject completed-run fixture";
  await view.evaluate(`(() => {
    const record={id:'resize-fixture',category:'debug',label:'Large diagnostic fixture',state:'available',value:{
      entries:Array.from({length:${Number(process.env.AWORKIT_QA_RECORDS ?? 1000)}},(_,i)=>({index:i,text:'Diagnostic value with whitespace and escaped lines.\\n'.repeat(160)})),
      endMarker:'END_OF_COMPLETE_JSON'
    }};
    const callbacks=window.__TAURI_INTERNALS__.callbacks, register=callbacks.set.bind(callbacks);
    callbacks.set=(id,callback)=>register(id,result=>{
      if(result?.chat && Array.isArray(result.evidence)) {result.chat.phase='completed';result.chat.title='Completed resize fixture';result.evidence=[record];}
      return callback(result);
    });
  })()`);
  await waitFor("document.querySelector('.chat-title-line').textContent.includes('Completed resize fixture')");
  stage = "open Raw JSON";
  await click('.inspector-tabs button:nth-child(2)');
  await waitFor("Boolean(document.querySelector('.run-details-json'))");
  await pause(300);
  stage = "resize Raw JSON";
  const raw = await view.evaluate("({renderedCharacters:document.querySelector('.run-details-json').textContent.length,whiteSpace:getComputedStyle(document.querySelector('.run-details-json')).whiteSpace})");
  await view.evaluate("window.rawMutations=0;new MutationObserver(m=>window.rawMutations+=m.length).observe(document.querySelector('.run-details-json'),{childList:true,subtree:true,characterData:true})");
  await view.command("Performance.enable");
  const before = await metrics(), started = performance.now(), at = await point('.inspector-splitter');
  const latencies = [];
  await view.command("Input.dispatchMouseEvent", { type: "mousePressed", ...at, button: "left", clickCount: 1 });
  for (let step = 1; step <= 12; step++) {
    const t = performance.now();
    await view.command("Input.dispatchMouseEvent", { type: "mouseMoved", x: at.x - step * 15, y: at.y, button: "left", buttons: 1 });
    await view.evaluate("new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(()=>resolve(document.querySelector('.run-details-inspector').clientWidth))))");
    latencies.push(performance.now() - t);
  }
  await view.command("Input.dispatchMouseEvent", { type: "mouseReleased", x: at.x - 180, y: at.y, button: "left", clickCount: 1 });
  const elapsedMs = performance.now() - started, after = await metrics();
  const report = { root, ...raw, elapsedMs, maxStepMs:Math.max(...latencies), latencies,
    durations:Object.fromEntries(['LayoutDuration','RecalcStyleDuration','ScriptDuration','TaskDuration'].map(k=>[k,after[k]-before[k]])),
    mutations:await view.evaluate('window.rawMutations'),
    width:await view.evaluate("document.querySelector('.run-details-inspector').clientWidth"),
  };
  await view.screenshot(resolve(root, "raw-json.png"));
  if (!process.env.AWORKIT_QA_BASELINE) {
    assert.ok(report.renderedCharacters <= 32768, 'Raw JSON layout must be bounded');
    assert.ok(report.maxStepMs < 500, 'Native drag must stay responsive');
    assert.ok(report.width >= 490, 'Divider must still resize the panel');
    await view.evaluate(`(() => {
      const e=document.querySelector('input[aria-label="JSON part"]');
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(e,e.max);
      e.dispatchEvent(new Event('input',{bubbles:true}));
    })()`);
    await waitFor("document.querySelector('.run-details-json').textContent.includes('END_OF_COMPLETE_JSON')");
    await pause(2100);
    assert.ok(await view.evaluate("document.querySelector('.run-details-json').textContent.includes('END_OF_COMPLETE_JSON')"), 'Polling must preserve the selected part');
    await view.screenshot(resolve(root, "last-json-part.png"));
  }
  report.passed = true;
  await writeFile(resolve(root, "result.json"), JSON.stringify(report, null, 2));
  console.log(JSON.stringify(report, null, 2));
} catch (error) {
  await writeFile(resolve(root, "failure.json"), JSON.stringify({ stage, error:String(error), logs }, null, 2));
  console.error(root, stage);
  throw error;
} finally {
  view?.close();
  if (child?.exitCode === null) { const exit = once(child, "exit"); child.kill(); await exit; }
}
