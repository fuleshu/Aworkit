// Exercises the built Windows WebView against an isolated profile and a local
// provider fixture: a model question is answered through the real dialog, and
// the answer must settle the pending tool call and continue the Run.
import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { once } from "node:events";
import assert from "node:assert/strict";

const root = resolve(`src-tauri/target/native-ask-user-${Date.now()}`);
const project = resolve(root, "project");
const answerKind = process.argv[2] ?? "option";
assert.ok(["option", "text", "skip"].includes(answerKind));
await mkdir(project, { recursive: true });
const requests = [];
const server = createServer(async (request, response) => {
  response.setHeader("Content-Type", "application/json");
  if (request.url === "/v1/models") return response.end(JSON.stringify({ data: [{ id: "ask-user-fixture" }] }));
  const chunks = []; for await (const chunk of request) chunks.push(chunk);
  const body = JSON.parse(Buffer.concat(chunks).toString());
  requests.push(body);
  let message;
  if (body.messages.some((entry) => entry.role === "tool")) {
    // The answer arrived: echo exactly what the tool loop delivered.
    const answer = body.messages.filter((entry) => entry.role === "tool").at(-1)?.content ?? "";
    message = { role: "assistant", content: `Recorded the answer: ${answer}` };
  } else {
    const name = body.tools?.find((tool) => tool.function.name === "ask_user")?.function.name;
    if (!name) throw new Error("Fixture expected the ask_user tool");
    message = {
      role: "assistant",
      content: null,
      tool_calls: [{
        id: "fixture.ask",
        type: "function",
        function: {
          name,
          arguments: JSON.stringify({
            prompt: "Which release channel should this build target?",
            title: "Release channel",
            options: [
              { id: "stable", label: "Stable" },
              { id: "beta", label: "Beta", description: "Early access" },
            ],
            allowFreeText: true,
          }),
        },
      }],
    };
  }
  const usage = { prompt_tokens: 17, completion_tokens: 9, total_tokens: 26 };
  if (body.stream) {
    const delta = message.tool_calls
      ? { role: "assistant", tool_calls: message.tool_calls.map((call, index) => ({ index, ...call })) }
      : { role: "assistant", content: message.content };
    for (const chunk of [
      { choices: [{ index: 0, delta, finish_reason: null }] },
      { choices: [{ index: 0, delta: {}, finish_reason: message.tool_calls ? "tool_calls" : "stop" }] },
      { choices: [], usage },
    ]) response.write(`data: ${JSON.stringify({ id: "fixture-response", object: "chat.completion.chunk", model: "ask-user-fixture", ...chunk })}\n\n`);
    response.end("data: [DONE]\n\n");
  } else {
    response.end(JSON.stringify({ id: "fixture-response", object: "chat.completion", model: "ask-user-fixture", choices: [{ index: 0, finish_reason: message.tool_calls ? "tool_calls" : "stop", message }], usage }));
  }
});
server.listen(0, "127.0.0.1");
await once(server, "listening");
const origin = `http://127.0.0.1:${server.address().port}`;
const port = 9241;
const child = spawn(resolve("src-tauri/target/debug/aworkit-desktop.exe"), [], {
  windowsHide: true,
  stdio: "ignore",
  env: { ...process.env, AWORKIT_QA_PROFILE: root, AWORKIT_QA_HIDE_WINDOW: "1", WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}` },
});
let socket;
try {
  let target;
  for (let attempt = 0; attempt < 200; attempt++) {
    try { target = (await fetch(`http://127.0.0.1:${port}/json/list`).then((response) => response.json())).find((entry) => entry.url.startsWith("http://tauri.localhost")); } catch {}
    if (target) break;
    await new Promise((resolve) => setTimeout(resolve, 150));
  }
  assert.ok(target, "Native WebView is available");
  socket = new WebSocket(target.webSocketDebuggerUrl);
  await once(socket, "open");
  let id = 0; const pending = new Map();
  socket.addEventListener("message", ({ data }) => {
    const response = JSON.parse(data);
    const call = pending.get(response.id);
    if (!call) return;
    pending.delete(response.id);
    response.error ? call.reject(response.error) : call.resolve(response.result);
  });
  const command = (method, params = {}) => new Promise((resolve, reject) => { const next = ++id; pending.set(next, { resolve, reject }); socket.send(JSON.stringify({ id: next, method, params })); });
  const evaluate = async (expression) => {
    const result = await command("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
    if (result.exceptionDetails) throw new Error(JSON.stringify(result.exceptionDetails));
    return result.result.value;
  };
  const waitFor = async (expression, attempts = 200) => {
    for (let attempt = 0; attempt < attempts; attempt++) {
      if (await evaluate(expression)) return;
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    throw new Error(`Timed out: ${expression}\n${await evaluate("document.body.innerText")}\n${JSON.stringify(await evaluate("window.__aworkitCalls ?? []"))}`);
  };
  const click = async (name) => {
    await waitFor(`(() => { const button = [...document.querySelectorAll('button')].find(button => (button.getAttribute('aria-label') === ${JSON.stringify(name)} || button.title === ${JSON.stringify(name)} || button.textContent.trim() === ${JSON.stringify(name)}) && !button.disabled); if (!button) return false; button.click(); return true; })()`);
  };
  const select = async (label, value) => {
    await waitFor(`Boolean(document.querySelector('select[aria-label="${label}"]:not(:disabled)'))`);
    return evaluate(`(() => { const input = document.querySelector('select[aria-label="${label}"]'); Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(input, ${JSON.stringify(value)}); input.dispatchEvent(new Event('change', { bubbles: true })); })()`);
  };
  const type = async (label, value) => {
    await waitFor(`Boolean(document.querySelector('textarea[aria-label="${label}"]:not(:disabled)'))`);
    return evaluate(`(() => { const input = document.querySelector('textarea[aria-label="${label}"]'); Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set.call(input, ${JSON.stringify(value)}); input.dispatchEvent(new Event('input', { bubbles: true })); })()`);
  };
  const snapshot = () => evaluate("window.__TAURI_INTERNALS__.invoke('desktop_snapshot', {afterSequence:0})");
  await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke)");
  await evaluate(`(async () => {
    const invoke = window.__TAURI_INTERNALS__.invoke;
    const settings = await invoke('settings_snapshot');
    await invoke('settings_commit', { command: { commandId:'native.ask.configure', expectedVersion:settings.version, appearance:'system', portableHistoryEnabled:false, provider:{baseUrl:${JSON.stringify(origin + "/v1")}, model:'ask-user-fixture', credentialAction:'keep', apiKey:null} } });
    const v2 = await invoke('settings_v2_snapshot');
    for (const provider of v2.settings.providers) for (const model of provider.models) model.capabilities = ['text','tools'];
    for (const tool of v2.settings.tools) tool.enabled = true;
    v2.settings.projects = [{id:'project.ask',name:'Ask fixture',workspace:{kind:'local_directory',location:${JSON.stringify(project)}},defaultWorkflowId:'workflow.simple-chat',portableHistoryEnabled:false}];
    await invoke('settings_v2_commit', {command:{commandId:'native.ask.enable',expectedVersion:v2.version,settings:v2.settings}});
    const workflow = await invoke('workflow_snapshot',{workflowId:'workflow.simple-chat'});
    const agent = workflow.document.nodes.find(node => node.type === 'agent');
    agent.configuration.toolIds = ['tool.ask_user'];
    await invoke('workflow_commit',{command:{commandId:'native.ask.workflow',expectedVersion:workflow.version,workflowId:'workflow.simple-chat',document:workflow.document}});
  })()`);
  await command("Page.reload");
  await waitFor("Boolean(window.__TAURI_INTERNALS__?.invoke)");
  const previous = (await snapshot()).chat.chatId;
  await click("Create a new Chat");
  await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(value => value.chat.chatId !== ${JSON.stringify(previous)} && value.chat.phase === 'draft')`);
  await select("Workflow for the first Chat input", "workflow.simple-chat");
  await select("Project for the first Chat input", "project.ask");
  await type("Chat input", "Ask me which channel to target.");
  await click("Send");
  // The question must raise its dialog, and the option plus Submit must work.
  await waitFor("Boolean(document.querySelector('dialog[open]'))", 300);
  const dialogText = await evaluate("document.querySelector('dialog[open]')?.textContent ?? ''");
  assert.match(dialogText, /Which release channel should this build target\?/);
  if (answerKind === "option") {
    await waitFor("Boolean(document.querySelector('dialog[open] input[value=beta]:not(:disabled)'))");
    await evaluate(`(() => {
      const radio = [...document.querySelectorAll('dialog[open] input[type=radio]')].find(input => input.value === 'beta');
      if (!radio) throw new Error('no beta radio');
      radio.click();
    })()`);
  } else if (answerKind === "text") {
    await type("Your own answer", "Use the preview channel");
  }
  await click(answerKind === "skip" ? "Skip" : "Submit answer");
  // The dialog closes immediately, and the answer settles the pending call.
  await waitFor("!document.querySelector('dialog[open]')", 100);
  const answerEvent = answerKind === "skip" ? "question.cancelled" : "question.answered";
  await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(value => value.events.some(event => event.kind === ${JSON.stringify(answerEvent)}))`, 300);
  await waitFor(`window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}).then(value => value.chat.phase === 'waiting_input' && value.events.some(event => event.kind === 'message.assistant'))`, 300);
  const toolResult = requests.at(-1)?.messages.find(entry => entry.role === "tool");
  assert.ok(toolResult, "the model received the answer");
  const delivered = JSON.parse(toolResult.content);
  if (answerKind === "option") assert.equal(delivered.optionId, "beta");
  if (answerKind === "text") assert.equal(delivered.freeText, "Use the preview channel");
  if (answerKind === "skip") assert.equal(delivered.cancelled, true);
  const settled = await snapshot();
  assert.equal(settled.events.filter(event => event.kind === answerEvent).length, 1);
  assert.ok(settled.events.some(event => event.kind === 'span.completed' && event.payload.spanId === 'span.tool.fixture.ask'), "the tool card settled");
  assert.equal(requests.length, 2, "one question and one continuation");
  const screenshot = await command("Page.captureScreenshot", { format: "png" });
  await writeFile(resolve(root, "result.png"), Buffer.from(screenshot.data, "base64"));
  await writeFile(resolve(root, "result.json"), JSON.stringify({ ok: true, root, answerKind, delivered, phase: settled.chat.phase, events: settled.events }, null, 2));
  console.log(JSON.stringify({ ok: true, root, answerKind, requests: requests.length }));
} finally {
  socket?.close(); child.kill(); server.close();
}
