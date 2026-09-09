// Verify grouped search choices and persisted billing modes in an isolated native WebView.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { connectNativeWebView } from "./native-webview.mjs";

const root = resolve(`src-tauri/target/web-search-settings-qa-${Date.now()}`);
await mkdir(root, { recursive: true });
const port = 9287;
const launch = () => spawn(resolve("src-tauri/target/debug/aworkit-desktop.exe"), [], {
  windowsHide: true, stdio: "ignore", env: {
    ...process.env, AWORKIT_QA_PROFILE: root, AWORKIT_QA_HIDE_WINDOW: "1",
    WEBVIEW2_USER_DATA_FOLDER: resolve(root, "webview"),
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}`,
  },
});
let child = launch(), view, credentialRef;
async function connect() {
  for (let attempt = 0; attempt < 150; attempt++) {
    try { return await connectNativeWebView(`http://127.0.0.1:${port}`); } catch {}
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  throw new Error("Native WebView did not start");
}
const evaluate = expression => view.evaluate(expression);
async function waitFor(expression) {
  for (let attempt = 0; attempt < 150; attempt++) {
    if (await evaluate(expression)) return;
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  throw new Error(`Timed out: ${expression}`);
}
const click = text => waitFor(`(() => {
  const button = [...document.querySelectorAll('button')].find(button =>
    !button.disabled && (button.textContent.trim() === ${JSON.stringify(text)} ||
    button.getAttribute('aria-label') === ${JSON.stringify(text)}));
  if (!button) return false; button.click(); return true;
})()`);
async function setValue(id, value) {
  await waitFor(`Boolean(document.getElementById(${JSON.stringify(id)}))`);
  await evaluate(`(() => {
    const input = document.getElementById(${JSON.stringify(id)});
    const prototype = input.tagName === 'SELECT' ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(prototype, 'value').set.call(input, ${JSON.stringify(value)});
    input.dispatchEvent(new Event(input.tagName === 'SELECT' ? 'change' : 'input', { bubbles: true }));
  })()`);
}
async function openSettings() {
  await waitFor("[...document.querySelectorAll('button')].some(button => button.textContent.includes('Settings'))");
  await evaluate("[...document.querySelectorAll('button')].find(button => button.textContent.includes('Settings')).click()");
  await waitFor("[...document.querySelectorAll('button')].some(button => button.textContent.trim().startsWith('Tools'))");
  await evaluate("[...document.querySelectorAll('button')].find(button => button.textContent.trim().startsWith('Tools')).click()");
  await waitFor("Boolean(document.getElementById('tool.web_search-backend'))");
}
const toolSnapshotExpression = "window.__TAURI_INTERNALS__.invoke('settings_v2_snapshot').then(snapshot => snapshot.settings.tools.find(tool => tool.id === 'tool.web_search'))";
async function save(expected) {
  await click("Save configuration");
  await waitFor(`${toolSnapshotExpression}.then(tool => Object.entries(${JSON.stringify(expected)}).every(([key, value]) => tool.configuration[key] === value))`);
  return evaluate(toolSnapshotExpression);
}
async function stop() {
  view?.close(); view = undefined;
  if (child && child.exitCode === null) {
    const exited = new Promise(resolve => child.once("exit", resolve));
    child.kill();
    await exited;
  }
}
try {
  view = await connect();
  await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke)");
  assert.equal((await evaluate(toolSnapshotExpression)).configuration.backend, "keyless");
  // A synthetic key allows native credential validation; no search requests are made.
  credentialRef = await evaluate(`(async () => {
    const invoke = window.__TAURI_INTERNALS__.invoke;
    const snapshot = await invoke('settings_v2_snapshot');
    const receipt = await invoke('settings_v2_store_credential', { command: {
      commandId: 'search.qa.credential', expectedVersion: snapshot.version,
      replaceCredentialRef: null, label: 'Search settings QA', kind: 'api_key',
      boundProviderId: null, boundEndpoint: null, fields: { api_key: 'synthetic-search-settings-key' },
    }});
    if (!receipt.accepted) throw new Error(receipt.reason);
    return receipt.credentialMutation.freshCredentialRef;
  })()`);
  await view.command("Page.reload");
  await openSettings();
  const groups = await evaluate(`Array.from(document.getElementById('tool.web_search-backend').children,
    group => ({ label: group.label, options: Array.from(group.children, option => option.textContent) }))`);
  assert.deepEqual(groups, [
    { label: "Automatic", options: ["Automatic (free only)", "Automatic (paid preferred)"] },
    { label: "Free providers", options: ["DuckDuckGo (free)", "Exa (free)", "Parallel (free)", "Firecrawl (free)", "Tavily (free)", "Keenable (free)"] },
    { label: "Self-hosted", options: ["SearXNG (self-hosted)"] },
    { label: "Paid providers", options: ["Exa (paid)", "Parallel (paid)", "Firecrawl (paid)", "Tavily (paid)", "Keenable (paid)", "Brave Search (paid)", "Grok by xAI (paid)", "DeepSeek (paid)"] },
  ]);
  assert.equal(await evaluate("document.getElementById('tool.web_search-provider-tier') === null"), true);

  await setValue("tool.web_search-backend", "exa:paid");
  await setValue("tool.web_search-provider-credential", credentialRef);
  await evaluate("document.getElementById('tool.web_search-keyless-rescue').click()");
  const paid = await save({ backend: "exa", providerTier: "paid", keylessFallback: false, keylessRescue: false });
  assert.equal(paid.credentialBindings[0].credentialRef, credentialRef);

  await setValue("tool.web_search-backend", "exa:free");
  const free = await save({ backend: "exa", providerTier: "free", keylessFallback: true });
  assert.deepEqual(free.credentialBindings, []);
  assert.equal(await evaluate("document.getElementById('tool.web_search-provider-credential') === null"), true);
  await view.command("Page.reload");
  await openSettings();
  assert.equal(await evaluate("document.getElementById('tool.web_search-backend').value"), "exa:free");

  await setValue("tool.web_search-backend", "automatic");
  await setValue("tool.web_search-provider-credential", credentialRef);
  assert.equal(await evaluate("document.getElementById('tool.web_search-credential-backend').value"), "deepseek");
  await evaluate("document.getElementById('tool.web_search-keyless-rescue').click()");
  const automatic = await save({ backend: "automatic", providerTier: "automatic", keylessFallback: true, keylessRescue: true });
  assert.equal(automatic.credentialBindings[0].credentialRef, credentialRef);

  await setValue("tool.web_search-backend", "searxng");
  await setValue("tool.web_search-searxng-url", "http://127.0.0.1:8888/");
  const selfHosted = await save({ backend: "searxng", searxngBaseUrl: "http://127.0.0.1:8888/" });
  assert.deepEqual(selfHosted.credentialBindings, []);

  await setValue("tool.web_search-backend", "keyless");
  const automaticFree = await save({ backend: "keyless", keylessFallback: true });
  assert.deepEqual(automaticFree.credentialBindings, []);
  await stop();
  child = launch();
  view = await connect();
  await openSettings();
  assert.equal(await evaluate("document.getElementById('tool.web_search-backend').value"), "keyless");
  await evaluate("document.getElementById('tool.web_search-backend').closest('.settings-record').scrollIntoView({block: 'start'})");
  await view.screenshot(resolve(root, "search-settings.png"));
  const report = { ok: true, groups, paid: paid.configuration, free: free.configuration,
    automatic: automatic.configuration, selfHosted: selfHosted.configuration, restarted: true };
  await writeFile(resolve(root, "report.json"), JSON.stringify(report, null, 2));
  console.log(JSON.stringify({ ok: true, root, cases: ["groups", "paid and free billing", "credential removal", "fallback", "self-hosted", "save and restart"] }));
} finally {
  if (view && credentialRef) {
    // The successful path leaves the test key unbound, so remove it from the OS store.
    await evaluate(`(async () => {
      const invoke = window.__TAURI_INTERNALS__.invoke;
      const snapshot = await invoke('settings_v2_snapshot');
      const tool = snapshot.settings.tools.find(tool => tool.id === 'tool.web_search');
      tool.credentialBindings = []; tool.configuration.backend = 'keyless'; tool.configuration.providerTier = 'automatic';
      const commit = await invoke('settings_v2_commit', { command: {
        commandId: 'search.qa.cleanup-settings', expectedVersion: snapshot.version, settings: snapshot.settings,
      }});
      if (!commit.accepted) throw new Error(commit.reason);
      const receipt = await invoke('settings_v2_delete_credential', { command: {
        commandId: 'search.qa.cleanup-credential', expectedVersion: commit.currentVersion, credentialRef: ${JSON.stringify(credentialRef)},
      }});
      if (!receipt.accepted) throw new Error(receipt.reason);
    })()`).catch(error => console.error("Test credential cleanup failed:", error));
  }
  await stop();
}
