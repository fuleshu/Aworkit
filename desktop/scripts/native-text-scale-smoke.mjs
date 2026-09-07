// Rebuild dist and the native debug executable first. This test owns an isolated
// profile and local provider; it never changes the user's Windows preferences.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { copyFile, mkdir, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { resolve } from "node:path";
import { connectNativeWebView } from "./native-webview.mjs";

const root = resolve(`src-tauri/target/native-text-scale-${Date.now()}`);
await mkdir(root, { recursive: true });
const executable = resolve(root, "aworkit-desktop.exe");
await copyFile(resolve(process.env.AWORKIT_QA_BINARY ?? "src-tauri/target/debug/aworkit-desktop.exe"), executable);
const content = "## Scalable text\n\nA paragraph with **bold text**, *emphasis*, and a [link](https://example.com).\n\n| Title | Date |\n| --- | --- |\n| Table text | September 6 |\n\n- First list entry\n- Second list entry\n\nInline `code` and a code block:\n\n```text\nconsole output\n```";
const server = createServer(async (request, response) => {
  for await (const _chunk of request) { /* Drain local fixture input. */ }
  response.setHeader("Content-Type", "text/event-stream");
  response.end(
    `data: ${JSON.stringify({ choices: [{ index: 0, delta: { content }, finish_reason: null }] })}\n\n` +
    `data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: "stop" }], usage: { prompt_tokens: 8, completion_tokens: 60 } })}\n\n` +
    "data: [DONE]\n\n",
  );
});
server.listen(0, "127.0.0.1");
await once(server, "listening");
const origin = `http://127.0.0.1:${server.address().port}`;
const endpoint = process.env.AWORKIT_CDP_URL ?? "http://127.0.0.1:9238";
const child = spawn(executable, [], {
  windowsHide: true, stdio: ["ignore", "pipe", "pipe"],
  env: { ...process.env, AWORKIT_QA_PROFILE: root, AWORKIT_QA_HIDE_WINDOW: "1",
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${new URL(endpoint).port}` },
});
let nativeLog = "", view;
child.stdout.on("data", chunk => { nativeLog += chunk; });
child.stderr.on("data", chunk => { nativeLog += chunk; });
const delay = () => new Promise(resolve => setTimeout(resolve, 100));
const report = { screens: [], baselines: [] };
try {
  for (let attempt = 0; attempt < 300; attempt++) {
    try { view = await connectNativeWebView(endpoint); break; } catch { await delay(); }
  }
  assert.ok(view, `Native WebView starts: ${nativeLog}`);
  const waitFor = async expression => {
    for (let attempt = 0; attempt < 150; attempt++) {
      if (await view.evaluate(expression)) return;
      await delay();
    }
    throw new Error(`Timed out: ${expression}\n${await view.evaluate("document.body.innerText")}`);
  };
  await waitFor("document.documentElement.dataset.appearanceReady === 'true'");
  report.nativeSystemScale = await view.evaluate("window.__TAURI_INTERNALS__.invoke('native_system_text_scale')");
  report.devicePixelRatio = await view.evaluate("window.devicePixelRatio");
  assert.equal(await view.evaluate("Number(document.documentElement.style.getPropertyValue('--aw-system-text-scale'))"), report.nativeSystemScale);
  assert.ok(!nativeLog.includes("Windows text scaling is unavailable"), nativeLog);
  // Non-client menu geometry is materialized only when the window is shown.
  await view.evaluate("window.__TAURI_INTERNALS__.invoke('native_window_action', { action: 'show' })");
  await delay();
  report.menu100 = await view.evaluate("window.__TAURI_INTERNALS__.invoke('native_menu_font_metrics')");
  assert.ok(Math.abs(report.menu100.fontSize - 13 * report.nativeSystemScale) < 0.01);
  await view.evaluate(`(async () => {
    const invoke = window.__TAURI_INTERNALS__.invoke;
    const settings = await invoke('settings_snapshot');
    await invoke('settings_commit', { command: { commandId: 'qa.font.provider', expectedVersion: settings.version, appearance: 'system', portableHistoryEnabled: false, provider: { baseUrl: ${JSON.stringify(origin + "/v1")}, model: 'font-fixture', credentialAction: 'keep', apiKey: null } } });
    await invoke('workflow_set_default', { command: { commandId: 'qa.font.workflow', workflowId: 'workflow.simple-chat' } });
  })()`);
  await view.command("Page.reload");
  await waitFor("Boolean(document.querySelector('textarea[aria-label=\"Chat input\"]:not(:disabled)'))");
  await view.evaluate(`(() => {
    const input = document.querySelector('textarea[aria-label="Chat input"]');
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set.call(input, 'Show the typography fixture.');
    input.dispatchEvent(new Event('input', { bubbles: true }));
  })()`);
  await waitFor("[...document.querySelectorAll('button')].some(b => b.textContent.trim() === 'Send' && !b.disabled)");
  await view.evaluate("[...document.querySelectorAll('button')].find(b => b.textContent.trim() === 'Send').click()");
  await waitFor("Boolean(document.querySelector('.speech-bubble table'))");
  const button = text => view.evaluate(`[...document.querySelectorAll('button')].find(b => !b.closest('[hidden]') && (b.textContent.trim() === ${JSON.stringify(text)} || b.getAttribute('aria-label') === ${JSON.stringify(text)}))?.click()`);

  // Compare every rendered text element, including inherited text, controls and
  // vendor widgets. Keep element references so wrapping cannot change pairing.
  const capture = async () => view.evaluate(`(() => {
    window.fontElements = [...document.querySelectorAll('body *')].filter(e =>
      !['SCRIPT','STYLE','OPTION'].includes(e.tagName) && e.getClientRects().length &&
      ([...e.childNodes].some(n => n.nodeType === 3 && n.textContent.trim()) || ['INPUT','TEXTAREA','SELECT'].includes(e.tagName)));
    return window.fontElements.map(e => ({ tag: e.tagName, className: e.getAttribute('class'), text: e.textContent.slice(0, 60), size: parseFloat(getComputedStyle(e).fontSize) }));
  })()`);
  const compare = async (name, before, ratio) => {
    const after = await view.evaluate("window.fontElements.map(e => e.isConnected ? parseFloat(getComputedStyle(e).fontSize) : null)");
    // Transient status actions may expire while screenshots are captured.
    const failures = before.flatMap((item, i) => after[i] === null || Math.abs(after[i] / item.size - ratio) < 0.002 ? [] : [{ ...item, after: after[i] }]);
    assert.ok(after.filter(size => size !== null).length >= before.length * 0.9);
    assert.deepEqual(failures, [], `${name}: all fonts scale by ${ratio}`);
    report.screens.push({ name, elements: before.length, ratio });
  };
  const checkScreen = async name => {
    await view.evaluate("document.documentElement.style.setProperty('--aw-font-scale', '1')");
    const before = await capture();
    assert.ok(before.length > 15, `${name} has rendered text`);
    const tooSmall = before.filter(item => item.size < 12 * report.nativeSystemScale - 0.01);
    const tooLarge = before.filter(item => item.size > 16 * report.nativeSystemScale + 0.01);
    assert.deepEqual(tooSmall, [], `${name}: no text below the 12px caption baseline`);
    assert.deepEqual(tooLarge, [], `${name}: UI headings stay within the 16px title baseline`);
    report.baselines.push({ name, min: Math.min(...before.map(item => item.size)), max: Math.max(...before.map(item => item.size)) });
    if (name === 'chat') {
      report.chatBody = await view.evaluate("parseFloat(getComputedStyle(document.querySelector('.bubble-markdown p')).fontSize)");
      assert.ok(Math.abs(report.chatBody - 14 * report.nativeSystemScale) < 0.01);
    }
    await view.screenshot(resolve(root, `${name}-100.png`));
    await view.evaluate("document.documentElement.style.setProperty('--aw-font-scale', '1.3')");
    await delay();
    await compare(name, before, 1.3);
    await view.screenshot(resolve(root, `${name}-130.png`));
    const scaled = await capture();
    await view.evaluate(`window.__TAURI_INTERNALS__.invoke('plugin:event|emit', { event: 'aworkit:system-text-scale', payload: ${report.nativeSystemScale * 1.25} })`);
    await delay();
    await compare(`${name} with OS event`, scaled, 1.25);
    await view.screenshot(resolve(root, `${name}-combined.png`));
    await view.evaluate(`window.__TAURI_INTERNALS__.invoke('plugin:event|emit', { event: 'aworkit:system-text-scale', payload: ${report.nativeSystemScale} })`);
    await view.evaluate("document.documentElement.style.setProperty('--aw-font-scale', '1')");
  };
  await checkScreen("chat");
  if (!(await view.evaluate("Boolean(document.querySelector('.run-details-inspector'))"))) await button("Run details");
  await waitFor("Boolean(document.querySelector('.run-details-inspector'))");
  await checkScreen("run-details");
  await view.evaluate("[...document.querySelectorAll('nav[aria-label=\"Primary navigation\"] button')].find(b => b.textContent.includes('Settings')).click()");
  await waitFor("Boolean(document.querySelector('nav[aria-label=\"Settings sections\"]'))");
  await view.evaluate("[...document.querySelectorAll('nav[aria-label=\"Settings sections\"] button')].find(b => b.textContent.includes('Appearance')).click()");
  await waitFor("Boolean(document.querySelector('#appearance-font-scale'))");
  await checkScreen("settings");
  const beforePreview = await capture();
  await view.evaluate(`(() => {
    const select = document.querySelector('#appearance-font-scale');
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(select, '1.3');
    select.dispatchEvent(new Event('change', { bubbles: true }));
  })()`);
  await delay();
  await compare("Settings selector preview", beforePreview, 1.3);
  report.menu130 = await view.evaluate("window.__TAURI_INTERNALS__.invoke('native_menu_font_metrics')");
  assert.ok(Math.abs(report.menu130.fontSize / report.menu100.fontSize - 1.3) < 0.01, "Native menu follows the actual Settings preview");
  assert.ok(report.menu130.items.every((item, i) => item.height > report.menu100.items[i].height), "Native menu bar remeasures its height");
  assert.ok(report.menu130.items.every((item, i) => item.width > report.menu100.items[i].width && item.width > 30), "Native menu labels keep their full measured width");
  await button("Save configuration");
  await waitFor("document.querySelector('.notification-message')?.textContent === 'Settings saved.'");
  await view.command("Page.reload");
  await waitFor("document.documentElement.dataset.appearanceReady === 'true'");
  assert.equal(await view.evaluate("document.documentElement.style.getPropertyValue('--aw-font-scale')"), "1.3", "Saved scale survives native reload");
  report.menuReload = await view.evaluate("window.__TAURI_INTERNALS__.invoke('native_menu_font_metrics')");
  assert.equal(report.menuReload.fontSize, report.menu130.fontSize, "Native menu restores the saved scale");
  await view.evaluate("[...document.querySelectorAll('nav[aria-label=\"Primary navigation\"] button')].find(b => b.textContent.includes('Workflows')).click()");
  await waitFor("Boolean(document.querySelector('.react-flow'))");
  await checkScreen("workflows");
  report.ok = true;
  await writeFile(resolve(root, "report.json"), JSON.stringify(report, null, 2));
  console.log(JSON.stringify({ root, ...report }, null, 2));
  if (process.env.AWORKIT_QA_KEEP_OPEN === '1') {
    await view.command("Page.reload");
    await waitFor("document.documentElement.dataset.appearanceReady === 'true'");
    await view.evaluate("window.__TAURI_INTERNALS__.invoke('native_window_action', { action: 'show' })");
    console.log('Native typography QA window ready for visual and keyboard checks. Close this test window to finish.');
    await once(child, 'exit');
  }
} finally {
  await writeFile(resolve(root, "report.json"), JSON.stringify(report, null, 2));
  await writeFile(resolve(root, "native.log"), nativeLog);
  view?.close();
  child.kill();
  server.closeAllConnections();
  server.close();
}
