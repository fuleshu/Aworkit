// Rebuild the frontend and debug app first. Uses an isolated profile and a local
// provider fixture; one link click opens a harmless page in the default browser.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { copyFile, mkdir, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { resolve } from "node:path";
import { connectNativeWebView } from "./native-webview.mjs";

const root = resolve(`src-tauri/target/native-chat-links-${Date.now()}`);
await mkdir(root, { recursive: true });
const executable = resolve(root, "aworkit-desktop.exe");
await copyFile(resolve(process.env.AWORKIT_QA_BINARY ?? "src-tauri/target/debug/aworkit-desktop.exe"), executable);
const browserVisits = [];
let origin;
const server = createServer(async (request, response) => {
  if (request.url === "/opened-from-chat") {
    browserVisits.push({ userAgent: request.headers["user-agent"], url: request.url });
    response.setHeader("Content-Type", "text/html");
    return response.end("<title>Aworkit link check</title><p>Aworkit opened this link in your default browser. You can close this tab.</p>");
  }
  if (request.method !== "POST") {
    response.setHeader("Content-Type", "application/json");
    return response.end(JSON.stringify({ data: [{ id: "chat-links-fixture" }] }));
  }
  for await (const _chunk of request) { /* Drain the local provider request. */ }
  response.setHeader("Content-Type", "text/event-stream");
  const content = `Plain model output. [**Open browser check**](${origin}/opened-from-chat)`;
  response.end(
    `data: ${JSON.stringify({ choices: [{ index: 0, delta: { content }, finish_reason: null }] })}\n\n` +
    `data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: "stop" }], usage: { prompt_tokens: 8, completion_tokens: 12 } })}\n\n` +
    "data: [DONE]\n\n",
  );
});
server.listen(0, "127.0.0.1");
await once(server, "listening");
origin = `http://127.0.0.1:${server.address().port}`;
const endpoint = process.env.AWORKIT_CDP_URL ?? "http://127.0.0.1:9236";
const child = spawn(executable, [], {
  windowsHide: true,
  stdio: ["ignore", "pipe", "pipe"],
  env: {
    ...process.env,
    AWORKIT_QA_PROFILE: root,
    AWORKIT_QA_HIDE_WINDOW: "1",
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${new URL(endpoint).port}`,
  },
});
let nativeLog = "";
child.stdout.on("data", chunk => { nativeLog += chunk; });
child.stderr.on("data", chunk => { nativeLog += chunk; });
let view;
let discoveryError;
const delay = () => new Promise(resolve => setTimeout(resolve, 100));
try {
  for (let attempt = 0; attempt < 300; attempt++) {
    try { view = await connectNativeWebView(endpoint); break; } catch (error) { discoveryError = error; await delay(); }
  }
  assert.ok(view, `Isolated native WebView starts: ${discoveryError}; ${nativeLog}`);
  const waitFor = async expression => {
    for (let attempt = 0; attempt < 150; attempt++) {
      if (await view.evaluate(expression)) return;
      await delay();
    }
    throw new Error(`Timed out: ${expression}\n${await view.evaluate("document.body.innerText")}`);
  };
  await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke)");
  await view.evaluate(`(async () => {
    const invoke = window.__TAURI_INTERNALS__.invoke;
    const settings = await invoke('settings_snapshot');
    await invoke('settings_commit', { command: { commandId: 'qa.links.configure', expectedVersion: settings.version, appearance: 'system', portableHistoryEnabled: false, provider: { baseUrl: ${JSON.stringify(origin + "/v1")}, model: 'chat-links-fixture', credentialAction: 'keep', apiKey: null } } });
    await invoke('workflow_set_default', { command: { commandId: 'qa.links.workflow', workflowId: 'workflow.simple-chat' } });
  })()`);
  await view.command("Page.reload");
  await waitFor("Boolean(document.querySelector('textarea[aria-label=\"Chat input\"]:not(:disabled)'))");
  await view.evaluate(`(() => {
    const workflow = document.querySelector('select[aria-label="Workflow for the first Chat input"]');
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(workflow, 'workflow.simple-chat');
    workflow.dispatchEvent(new Event('change', { bubbles: true }));
    const input = document.querySelector('textarea[aria-label="Chat input"]');
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set.call(input, 'Show the local browser check link.');
    input.dispatchEvent(new Event('input', { bubbles: true }));
  })()`);
  await waitFor("[...document.querySelectorAll('button')].some(button => button.textContent.trim() === 'Send' && !button.disabled)");
  await view.evaluate("[...document.querySelectorAll('button')].find(button => button.textContent.trim() === 'Send').click()");
  await waitFor("Boolean(document.querySelector('.model-call-block:not([aria-busy]) .speech-bubble a'))");
  const selection = () => view.evaluate("[...document.querySelectorAll('.timeline-scroll .selected')].map(item => item.getAttribute('aria-label'))");
  const initialSelection = await selection();
  // CDP pointer events exercise the actual summary marker and nested link text.
  const click = async (selector, marker = false) => {
    await view.evaluate(`document.querySelector(${JSON.stringify(selector)}).scrollIntoView({ block: 'center' })`);
    await delay();
    const point = await view.evaluate(`(() => {
      const bounds = document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect();
      return { x: bounds.left + ${marker ? "7" : "bounds.width / 2"}, y: bounds.top + bounds.height / 2 };
    })()`);
    await view.command("Input.dispatchMouseEvent", { type: "mousePressed", ...point, button: "left", clickCount: 1 });
    await view.command("Input.dispatchMouseEvent", { type: "mouseReleased", ...point, button: "left", clickCount: 1 });
    await delay();
  };
  for (const index of [0, 1]) {
    const selector = `.model-call-data:nth-of-type(${index + 1}) > summary`;
    await click(selector, true);
    assert.equal(await view.evaluate(`document.querySelector(${JSON.stringify(selector)}).parentElement.open`), true);
    assert.deepEqual(await selection(), initialSelection);
    await click(selector, true);
    assert.equal(await view.evaluate(`document.querySelector(${JSON.stringify(selector)}).parentElement.open`), false);
    assert.deepEqual(await selection(), initialSelection);
  }
  assert.equal(browserVisits.length, 0, "Rendering a citation never opens it");
  await click(".model-call-block .speech-bubble a strong");
  for (let attempt = 0; attempt < 150 && browserVisits.length === 0; attempt++) await delay();
  assert.equal(browserVisits.length, 1, "Default browser reached the exact clicked URL once");
  assert.deepEqual(await selection(), initialSelection);
  assert.ok((await view.evaluate("location.href")).startsWith("http://tauri.localhost"));
  await click(".model-call-heading strong");
  assert.equal(await view.evaluate("Boolean(document.querySelector('.model-call-block.selected'))"), true);
  await view.screenshot(resolve(root, "native-chat-links.png"));
  const report = { ok: true, disclosureClicks: 4, openedUrl: `${origin}/opened-from-chat`, browserVisits, plainContentSelects: true };
  await writeFile(resolve(root, "report.json"), JSON.stringify(report, null, 2));
  console.log(JSON.stringify({ root, ...report }, null, 2));
} finally {
  await writeFile(resolve(root, "native.log"), nativeLog);
  view?.close();
  child.kill();
  server.closeAllConnections();
  server.close();
}
