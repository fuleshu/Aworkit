// Exercise real Settings controls and persistence in an isolated Windows WebView.
import { spawn } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { once } from "node:events";
import assert from "node:assert/strict";

const root = resolve(`src-tauri/target/web-settings-qa-${Date.now()}`);
await mkdir(root, { recursive: true });
const port = 9246;
const child = spawn(resolve("src-tauri/target/debug/aworkit-desktop.exe"), [], {
  windowsHide: true, stdio: "ignore", env: { ...process.env, AWORKIT_QA_PROFILE: root,
    AWORKIT_QA_HIDE_WINDOW: "1", WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}` },
});
let socket;
try {
  let target;
  for (let attempt = 0; attempt < 100; attempt++) {
    try { target = (await fetch(`http://127.0.0.1:${port}/json/list`).then(r => r.json())).find(t => t.url.startsWith("http://tauri.localhost")); } catch {}
    if (target) break;
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  assert.ok(target, "Native WebView started");
  socket = new WebSocket(target.webSocketDebuggerUrl);
  await once(socket, "open");
  let nextId = 0;
  const pending = new Map();
  socket.addEventListener("message", ({ data }) => {
    const message = JSON.parse(data), call = pending.get(message.id);
    if (!call) return;
    pending.delete(message.id);
    message.error ? call.reject(message.error) : call.resolve(message.result);
  });
  const command = (method, params = {}) => new Promise((resolve, reject) => {
    const id = ++nextId; pending.set(id, { resolve, reject }); socket.send(JSON.stringify({ id, method, params }));
  });
  const evaluate = async expression => {
    const result = await command("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
    if (result.exceptionDetails) throw new Error(JSON.stringify(result.exceptionDetails));
    return result.result.value;
  };
  const waitFor = async expression => {
    for (let attempt = 0; attempt < 100; attempt++) {
      if (await evaluate(expression)) return;
      await new Promise(resolve => setTimeout(resolve, 100));
    }
    throw new Error(`Timed out: ${expression}`);
  };
  await waitFor("[...document.querySelectorAll('button')].some(b=>b.textContent.includes('Settings'))");
  await evaluate("[...document.querySelectorAll('button')].find(b=>b.textContent.includes('Settings')).click()");
  await waitFor("[...document.querySelectorAll('button')].some(b=>b.textContent.trim().startsWith('Tools'))");
  await evaluate("[...document.querySelectorAll('button')].find(b=>b.textContent.trim().startsWith('Tools')).click()");
  await waitFor("Boolean(document.getElementById('tool.web_fetch-maximum-download-bytes'))");
  const before = await evaluate(`['tool.web_fetch','tool.web_extract'].map(id => {
    const download=document.getElementById(id+'-maximum-download-bytes');
    const preview=document.getElementById(id+'-maximum-preview-bytes');
    const render=document.getElementById(id+'-render-when-needed');
    return {id,download:download.value,maximum:download.max,preview:preview.value,render:render.checked,tooltips:[download,preview,render].every(e=>e.title.length>20)};
  })`);
  assert.ok(before.every(value => value.download === "8388608" && value.maximum === "8388608" && value.preview === "32768" && value.render && value.tooltips));
  await evaluate(`(() => {
    const input=document.getElementById('tool.web_fetch-maximum-download-bytes');
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(input,'2097152');
    input.dispatchEvent(new Event('input',{bubbles:true}));
    document.getElementById('tool.web_fetch-render-when-needed').click();
  })()`);
  await waitFor("[...document.querySelectorAll('button')].some(b=>b.textContent.trim()==='Save configuration'&&!b.disabled)");
  await evaluate("[...document.querySelectorAll('button')].find(b=>b.textContent.trim()==='Save configuration').click()");
  await waitFor("window.__TAURI_INTERNALS__.invoke('settings_v2_snapshot').then(s=>s.settings.tools.find(t=>t.id==='tool.web_fetch').configuration.renderWhenNeeded===false)");
  const after = await evaluate("window.__TAURI_INTERNALS__.invoke('settings_v2_snapshot').then(s=>s.settings.tools.find(t=>t.id==='tool.web_fetch').configuration)");
  assert.equal(after.maximumDownloadBytes, 2097152);
  assert.equal(after.renderWhenNeeded, false);
  await evaluate("document.getElementById('tool.web_fetch-maximum-download-bytes').closest('section').scrollIntoView({block:'start'})");
  const screenshot = await command("Page.captureScreenshot", { format: "png" });
  await writeFile(resolve(root, "settings.png"), Buffer.from(screenshot.data, "base64"));
  await writeFile(resolve(root, "report.json"), JSON.stringify({ok:true,before,after},null,2));
  console.log(JSON.stringify({ok:true,root,before,after},null,2));
} finally {
  socket?.close();
  child.kill();
}
