// Real WebView regression: measured nodes remain painted after selection and edits.
// All workflow edits stay in an isolated QA profile; no provider is configured.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { createServer } from "node:net";
import { connectNativeWebView } from "./native-webview.mjs";

const root = resolve("src-tauri/target/native-workflow-canvas-" + Date.now());
await mkdir(root, { recursive: true });
const listener = createServer();
await new Promise(resolve => listener.listen(0, "127.0.0.1", resolve));
const debuggerPort = listener.address().port;
await new Promise(resolve => listener.close(resolve));
const endpoint = "http://127.0.0.1:" + debuggerPort;
const child = spawn(resolve(process.env.AWORKIT_QA_EXE ?? "src-tauri/target/debug/aworkit-desktop.exe"), [], {
  windowsHide: true, stdio: ["ignore", "pipe", "pipe"], env: { ...process.env, AWORKIT_QA_PROFILE: root,
    WEBVIEW2_USER_DATA_FOLDER: resolve(root, "webview"),
    AWORKIT_QA_HIDE_WINDOW: "1", WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: "--remote-debugging-port=" + debuggerPort },
});
let processOutput = "";
child.stdout.on("data", chunk => { processOutput += chunk; });
child.stderr.on("data", chunk => { processOutput += chunk; });
let view;
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
async function waitFor(expression) {
  for (let i = 0; i < 120; i++) {
    if (await view.evaluate(`Boolean(${expression})`)) return;
    await pause(100);
  }
  throw new Error("Timed out: " + expression);
}
const click = text => view.evaluate(`(() => {
  const name = ${JSON.stringify(text)};
  const b = [...document.querySelectorAll('button')].find(b => (b.textContent.trim() === name || b.title === name || b.getAttribute('aria-label') === name) && b.getClientRects().length);
  if (!b) throw new Error('Missing button: ' + ${JSON.stringify(text)}); b.click();
})()`);
const canvasState = `(() => {
  const canvas = document.querySelector('.graph-surface').getBoundingClientRect();
  const nodes = [...document.querySelectorAll('.react-flow__node')].map(n => {
    const rect = n.getBoundingClientRect();
    return { id:n.dataset.id, painted:getComputedStyle(n).visibility !== 'hidden' && rect.width > 0 && rect.height > 0,
      visible:getComputedStyle(n).visibility !== 'hidden' &&
      rect.width > 0 && rect.height > 0 && rect.left >= canvas.left - 1 && rect.right <= canvas.right + 1 &&
      rect.top >= canvas.top - 1 && rect.bottom <= canvas.bottom + 1 };
  });
  return { nodes, transform:document.querySelector('.react-flow__viewport').style.transform,
    edges:document.querySelectorAll('.react-flow__edge').length };
})()`;
const cases = [];
async function assertPainted(name, count) {
  await pause(500);
  const state = await view.evaluate(canvasState);
  await view.screenshot(resolve(root, name + ".png"));
  cases.push({ name, ...state });
  assert.equal(state.nodes.filter(n => n.painted).length, count, `${name}: all ${count} nodes must remain painted, not just the selected node`);
  assert.equal(state.edges, count - 1, `${name}: every transition is rendered`);
}
try {
  let connectionError;
  for (let i = 0; i < 100; i++) {
    try { view = await connectNativeWebView(endpoint, process.env.AWORKIT_QA_PAGE_URL ?? "http://tauri.localhost"); break; } catch (error) { connectionError=String(error); await pause(150); }
  }
  if (!view) throw new Error(`Native WebView did not start (exit ${child.exitCode}): ${connectionError}; ${processOutput}`);
  await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke)");
  await waitFor("[...document.querySelectorAll('nav[aria-label=\"Primary navigation\"] button')].some(b=>b.textContent.includes('Workflows'))");
  await click("◇ Workflows").catch(() => view.evaluate("[...document.querySelectorAll('nav[aria-label=\"Primary navigation\"] button')].find(b=>b.textContent.includes('Workflows')).click()"));
  await waitFor("document.querySelector('.workflow-library-bar select') && document.body.innerText.includes('Version 1')");
  const initialId = await view.evaluate("document.querySelector('.workflow-library-bar select').value");
  // Hidden QA WebViews can defer their first ResizeObserver delivery. Wait for
  // that initial paint before testing whether editor updates lose measurements.
  await waitFor("document.querySelectorAll('.react-flow__node').length > 0 && [...document.querySelectorAll('.react-flow__node')].every(n=>getComputedStyle(n).visibility !== 'hidden')");
  await assertPainted("initial-load", initialId === "workflow.simple-chat" ? 4 : 5);
  for (const label of ['Agent', 'Output', 'Input']) {
    await view.evaluate(`[...document.querySelectorAll('[aria-label="Workflow nodes"] button')].find(b=>b.textContent===${JSON.stringify(label)}).click()`);
    await assertPainted("selected-" + label, initialId === "workflow.simple-chat" ? 4 : 5);
  }
  const otherId = initialId === "workflow.simple-chat" ? "workflow.standard-agent" : "workflow.simple-chat";
  await view.evaluate(`(() => {const e=document.querySelector('.workflow-library-bar select');
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype,'value').set.call(e,${JSON.stringify(otherId)});
    e.dispatchEvent(new Event('change',{bubbles:true}));})()`);
  await waitFor(`document.querySelector('.workflow-library-bar select').value === ${JSON.stringify(otherId)}`);
  await assertPainted("switched-workflow", otherId === "workflow.simple-chat" ? 4 : 5);
  // Keep the exact viewport, selection, and undo state across a hidden route.
  await view.evaluate("document.querySelector('.react-flow__controls-zoomout').click(); document.querySelector('.react-flow__node[data-id=\"agent.1\"]').click()");
  await waitFor("Boolean(document.querySelector('input[title=\"Edit the selected node label\"]'))");
  await view.evaluate(`(() => {const e=document.querySelector('input[title="Edit the selected node label"]');
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(e,'Retained Agent');
    e.dispatchEvent(new Event('input',{bubbles:true}));})()`);
  await pause(400);
  await assertPainted("label-edit", otherId === "workflow.simple-chat" ? 4 : 5);
  const before = await view.evaluate(canvasState);
  await view.evaluate("[...document.querySelectorAll('nav[aria-label=\"Primary navigation\"] button')].find(b=>b.textContent.includes('Settings')).click()");
  await waitFor("document.querySelector('[data-route=\"settings\"]:not([hidden])')");
  await click("Back to Workflows");
  await waitFor("document.querySelector('[data-route=\"workflows\"]:not([hidden])')");
  await pause(400);
  const after = await view.evaluate(canvasState);
  assert.equal(after.transform, before.transform, "Settings return keeps pan and zoom");
  assert.equal(await view.evaluate("document.querySelector('input[title=\"Edit the selected node label\"]').value"), "Retained Agent");
  assert.equal(await view.evaluate("[...document.querySelectorAll('button')].find(b=>b.textContent.trim().endsWith('Undo')).disabled"), false);
  cases.push({name:"settings-return",...after});
  await view.screenshot(resolve(root,"settings-return.png"));
  await assertPainted("settings-return-painted", otherId === "workflow.simple-chat" ? 4 : 5);
  await writeFile(resolve(root,"report.json"),JSON.stringify({ok:true,root,cases},null,2));
  console.log(JSON.stringify({ok:true,root,cases:cases.map(c=>c.name)}));
} catch (error) {
  await writeFile(resolve(root,"failure.json"),JSON.stringify({error:String(error),cases},null,2));
  console.error(root);
  throw error;
} finally {view?.close();child.kill();}
