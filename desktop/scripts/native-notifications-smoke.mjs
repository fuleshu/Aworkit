// Run against a debug app launched with AWORKIT_QA_PROFILE and a dedicated CDP port.
// This test edits only that profile's appearance settings. It never sends a Chat.
import { mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { connectNativeWebView } from "./native-webview.mjs";

if (process.env.AWORKIT_SETTINGS_QA !== "1") {
  throw new Error("Launch an isolated AWORKIT_QA_PROFILE, then set AWORKIT_SETTINGS_QA=1 to run this Settings test.");
}
const webview = await connectNativeWebView(process.env.AWORKIT_CDP_URL ?? "http://127.0.0.1:9224");
const output = resolve("src-tauri/target/notification-qa");
await mkdir(output, { recursive: true });
const results = [];
const check = (name, value) => { results.push({ name, ...value }); if (!value.ok) throw new Error(`${name}: ${JSON.stringify(value)}`); };
try {
  await webview.command("Page.reload");
  for (let attempt = 0; attempt < 100; attempt++) {
    if (await webview.evaluate(`!!document.querySelector('textarea[aria-label="Chat input"]')`)) break;
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  await webview.evaluate(`(() => {
    window.qa = {
      wait: async (predicate) => {
        for (let i = 0; i < 100; i++) { if (predicate()) return; await new Promise(resolve => setTimeout(resolve, 100)); }
        throw new Error('Timed out waiting for native UI');
      },
      button: (text, root = document) => [...root.querySelectorAll('button')].find(button => !button.closest('[hidden]') && (button.textContent.trim() === text || button.textContent.trim() === '↶ ' + text || button.getAttribute('aria-label') === text)),
      settings: () => [...document.querySelectorAll('nav[aria-label="Primary navigation"] button')].find(button => button.textContent.includes('Settings')).click(),
      rect: selector => { const rect = document.querySelector(selector).getBoundingClientRect(); return { top: rect.top, bottom: rect.bottom, height: rect.height, width: rect.width }; },
    };
  })()`);

  check("Chat Back preserves draft, caret, focus and inspector", await webview.evaluate(`(async () => {
    const composer = document.querySelector('textarea[aria-label="Chat input"]');
    if (!composer) throw new Error('Native composer missing');
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set.call(composer, 'QA unsent draft with a selected word');
    composer.dispatchEvent(new Event('input', { bubbles: true }));
    const splitter = document.querySelector('[aria-label="Resize Run details"]');
    splitter?.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowLeft', bubbles: true }));
    await new Promise(resolve => setTimeout(resolve, 50));
    const width = splitter?.getAttribute('aria-valuenow');
    composer.focus(); composer.setSelectionRange(3, 9);
    composer.dispatchEvent(new KeyboardEvent('keydown', { key: ',', ctrlKey: true, bubbles: true }));
    await qa.wait(() => qa.button('Back to Chat'));
    qa.settings();
    qa.button('Back to Chat').click();
    await qa.wait(() => document.activeElement === composer);
    return { ok: composer.value === 'QA unsent draft with a selected word' && composer.selectionStart === 3 && composer.selectionEnd === 9 && width === splitter?.getAttribute('aria-valuenow'), caret: [composer.selectionStart, composer.selectionEnd], width };
  })()`));

  check("Native Settings save expires after five seconds", await webview.evaluate(`(async () => {
    qa.settings(); await qa.wait(() => qa.button('Back to Chat'));
    [...document.querySelectorAll('nav[aria-label="Settings sections"] button')].find(button => button.textContent.includes('Appearance')).click();
    await qa.wait(() => document.querySelector('input[title="Preview dark color mode"]'));
    const dark = document.querySelector('input[title="Preview dark color mode"]');
    const next = dark.checked ? document.querySelector('input[title="Preview light color mode"]') : dark;
    next.click(); await qa.wait(() => !qa.button('Save configuration').disabled);
    qa.button('Save configuration').click();
    await qa.wait(() => document.querySelector('.notification-message').textContent === 'Settings saved.');
    const before = qa.rect('.main-surface');
    await new Promise(resolve => setTimeout(resolve, 5_300));
    const after = qa.rect('.main-surface');
    return { ok: document.querySelector('.notification-message').textContent === 'Ready' && before.height === after.height, before, after };
  })()`));

  check("Save and return leaves after a verified native commit", await webview.evaluate(`(async () => {
    const saved = document.documentElement.dataset.appearance;
    const next = saved === 'dark' ? 'light' : 'dark';
    document.querySelector('input[title="Preview ' + next + ' color mode"]').click();
    qa.button('Back to Chat').click(); await qa.wait(() => qa.button('Save and return'));
    qa.button('Save and return').click(); await qa.wait(() => !qa.button('Back to Chat'));
    const returned = !document.querySelector('[role="dialog"]') && document.documentElement.dataset.appearance === next;
    qa.settings(); await qa.wait(() => qa.button('Back to Chat'));
    await qa.wait(() => !!document.querySelector('input[title="Preview ' + next + ' color mode"]'));
    return { ok: returned && document.querySelector('input[title="Preview ' + next + ' color mode"]').checked };
  })()`));

  check("Dirty Settings Stay and Discard restore committed appearance", await webview.evaluate(`(async () => {
    const saved = document.documentElement.dataset.appearance;
    document.querySelector('input[title="Preview ' + (saved === 'dark' ? 'light' : 'dark') + ' color mode"]').click();
    qa.button('Back to Chat').click(); await qa.wait(() => qa.button('Stay in Settings'));
    qa.button('Stay in Settings').click();
    const stayed = !!qa.button('Back to Chat') && document.documentElement.dataset.appearance !== saved;
    qa.button('Back to Chat').click(); await qa.wait(() => qa.button('Discard and return'));
    qa.button('Discard and return').click(); await qa.wait(() => !qa.button('Back to Chat'));
    return { ok: stayed && document.documentElement.dataset.appearance === saved && document.querySelector('textarea[aria-label="Chat input"]').value.includes('QA unsent'), saved };
  })()`));

  check("Workflow selection, viewport and undo survive Settings", await webview.evaluate(`(async () => {
    [...document.querySelectorAll('nav[aria-label="Primary navigation"] button')].find(button => button.textContent.includes('Workflows')).click();
    await qa.wait(() => document.querySelector('.react-flow__node[data-id="agent.1"]'));
    await new Promise(resolve => setTimeout(resolve, 300));
    const node = document.querySelector('.react-flow__node[data-id="agent.1"]'); node.click();
    await qa.wait(() => document.querySelector('input[title="Edit the selected node label"]'));
    const label = document.querySelector('input[title="Edit the selected node label"]');
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(label, 'QA selected agent');
    label.dispatchEvent(new Event('input', { bubbles: true }));
    await qa.wait(() => qa.button('Undo') && !qa.button('Undo').disabled);
    const viewport = document.querySelector('.react-flow__viewport');
    const transform = viewport?.style.transform;
    const selected = node.classList.contains('selected');
    qa.settings(); await qa.wait(() => qa.button('Back to Workflows'));
    qa.button('Back to Workflows').click(); await qa.wait(() => !qa.button('Back to Workflows'));
    const preserved = document.querySelector('.react-flow__node[data-id="agent.1"]') === node && selected && node.classList.contains('selected') && label.value === 'QA selected agent' && viewport?.style.transform === transform && !qa.button('Undo').disabled;
    qa.button('Undo').click(); await new Promise(resolve => setTimeout(resolve, 100));
    return { ok: preserved && qa.button('Save').disabled, transform };
  })()`));

  check("Docked notification details reserve workspace space", await webview.evaluate(`(async () => {
    const closed = qa.rect('.main-surface');
    window.dispatchEvent(new CustomEvent('aworkit:native-presentation', { detail: { kind: 'notification', title: 'QA long notification ' + 'readable content '.repeat(25), body: 'Full diagnostic details stay in a docked panel.' } }));
    await qa.wait(() => document.querySelector('.notification-message').textContent.startsWith('QA long'));
    document.querySelector('.notification-list-toggle').click(); await qa.wait(() => document.querySelector('.notification-details'));
    const main = qa.rect('.main-surface'), dock = qa.rect('.desktop-status-dock'), bar = qa.rect('.desktop-status-bar');
    return { ok: main.height < closed.height && main.bottom <= dock.top + 1 && Math.abs(bar.bottom - innerHeight) < 1 && document.body.scrollHeight <= innerHeight, closed, main, dock, bar };
  })()`));
  await webview.screenshot(resolve(output, "desktop-notifications.png"));

  for (const theme of ["light", "dark"]) {
    await webview.command("Emulation.setDeviceMetricsOverride", { width: 760, height: 560, deviceScaleFactor: 1, mobile: false });
    check(`Native ${theme} layout at 200% text`, await webview.evaluate(`(async () => {
      qa.settings(); await qa.wait(() => qa.button('Back to Workflows'));
      [...document.querySelectorAll('nav[aria-label="Settings sections"] button')].find(button => button.textContent.includes('Appearance')).click();
      document.querySelector('input[title="Preview ${theme} color mode"]').click();
      await new Promise(resolve => setTimeout(resolve, 60));
      document.documentElement.style.setProperty('--aw-font-scale', '2');
      await new Promise(resolve => setTimeout(resolve, 100));
      const main = qa.rect('.main-surface'), dock = qa.rect('.desktop-status-dock'), bar = qa.rect('.desktop-status-bar');
      const toggle = qa.rect('.notification-list-toggle');
      return { ok: main.bottom <= dock.top + 1 && Math.abs(bar.bottom - innerHeight) < 1 && bar.height >= 64 && toggle.width > 0 && document.body.scrollHeight <= innerHeight, main, bar, theme: document.documentElement.dataset.appearance };
    })()`));
    await webview.screenshot(resolve(output, `desktop-notifications-${theme}-200.png`));
  }
  await webview.command("Emulation.clearDeviceMetricsOverride");
  await webview.evaluate(`(async () => {
    document.documentElement.style.setProperty('--aw-font-scale', '1');
    qa.button('Back to Workflows').click();
    await new Promise(resolve => setTimeout(resolve, 100));
    qa.button('Discard and return')?.click();
  })()`);
  console.log(JSON.stringify({ ok: true, results, output }, null, 2));
} finally {
  await writeFile(resolve(output, "results.json"), JSON.stringify(results, null, 2));
  webview.close();
}
