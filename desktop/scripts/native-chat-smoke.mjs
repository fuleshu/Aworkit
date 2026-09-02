import { mkdir, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";

const endpoint = process.env.AWORKIT_CDP_URL ?? "http://127.0.0.1:9223";
const screenshotPath = resolve(
  process.env.AWORKIT_SMOKE_SCREENSHOT ??
    "src-tauri/target/native-chat-smoke.png",
);

const targets = await fetch(`${endpoint}/json/list`).then((response) => {
  if (!response.ok) {
    throw new Error(`WebView target discovery failed with HTTP ${response.status}`);
  }
  return response.json();
});
const target = targets.find(
  (candidate) =>
    candidate.type === "page" && candidate.url.startsWith("http://tauri.localhost"),
);
if (target === undefined) {
  throw new Error("No running Aworkit WebView target was found");
}

const socket = new WebSocket(target.webSocketDebuggerUrl);
const pending = new Map();
let messageId = 0;

socket.addEventListener("message", ({ data }) => {
  const message = JSON.parse(data);
  if (message.id === undefined) return;
  const request = pending.get(message.id);
  if (request === undefined) return;
  pending.delete(message.id);
  if (message.error !== undefined) {
    request.reject(new Error(message.error.message));
  } else {
    request.resolve(message.result);
  }
});

await new Promise((resolveOpen, rejectOpen) => {
  socket.addEventListener("open", resolveOpen, { once: true });
  socket.addEventListener(
    "error",
    () => rejectOpen(new Error("Could not connect to the Aworkit WebView")),
    { once: true },
  );
});

function command(method, params = {}) {
  const id = ++messageId;
  return new Promise((resolveCommand, rejectCommand) => {
    pending.set(id, { resolve: resolveCommand, reject: rejectCommand });
    socket.send(JSON.stringify({ id, method, params }));
  });
}

async function evaluate(expression) {
  const result = await command("Runtime.evaluate", {
    expression,
    returnByValue: true,
    awaitPromise: true,
  });
  if (result.exceptionDetails !== undefined) {
    throw new Error(result.exceptionDetails.text ?? "WebView evaluation failed");
  }
  return result.result.value;
}

await command("Runtime.enable");
await command("Page.enable");
await evaluate(`(() => {
  const chatControl = [...document.querySelectorAll("button, a")].find(
    (element) => element.textContent?.trim() === "Chat",
  );
  chatControl?.click();
})()`);

let state;
for (let attempt = 0; attempt < 25; attempt += 1) {
  state = await evaluate(`(() => {
    const bodyText = document.body?.innerText ?? "";
    const composer = document.querySelector("textarea");
    return {
      readyState: document.readyState,
      title: document.title,
      projectionUnavailable: bodyText.includes("Chat projection unavailable"),
      obsoleteSnapshotError:
        bodyText.includes('path: [ "lastSequence" ]') ||
        bodyText.includes('path: [ "timeline" ]'),
      composerPresent: composer !== null,
      composerEnabled: composer !== null && !composer.disabled,
      newChatPresent: bodyText.includes("New Chat"),
      bodyPreview: bodyText.slice(0, 600),
    };
  })()`);
  if (state.projectionUnavailable || state.composerPresent) break;
  await new Promise((resolveWait) => setTimeout(resolveWait, 200));
}

const runDetailsState = await evaluate(`(() => {
  const inspector = document.querySelector('[aria-label="Run details"]');
  const content = inspector?.querySelector('.run-details-content');
  return {
    present: inspector !== null,
    entireRunPresent: inspector?.textContent?.includes('Entire run') === true,
    executionLogPresent: inspector?.textContent?.includes('Execution log') === true,
    detailsContainsRawJson: content?.querySelector('pre') !== null,
  };
})()`);
const modelCallOutputState = await evaluate(`(() => {
  const block = document.querySelector('.model-call-block');
  const output = [...(block?.querySelectorAll('details.model-call-data') ?? [])]
    .find((element) => element.querySelector('summary')?.textContent?.trim() === 'Output');
  const json = output?.querySelector('pre')?.textContent ?? null;
  if (json === null) {
    return { blockPresent: block !== null, outputPresent: false, validJson: false, duplicateTextKinds: [] };
  }
  try {
    const value = JSON.parse(json);
    const textKinds = Array.isArray(value)
      ? value.flatMap((entry) =>
          entry !== null && typeof entry === 'object' && typeof entry.kind === 'string' &&
          (typeof entry.text === 'string' || typeof entry.data === 'string')
            ? [entry.kind + ':' + (typeof entry.text === 'string' ? 'text' : 'data')]
            : [],
        )
      : [];
    const seen = new Set();
    const duplicates = new Set();
    for (const kind of textKinds) {
      if (seen.has(kind)) duplicates.add(kind);
      seen.add(kind);
    }
    return {
      blockPresent: true,
      outputPresent: true,
      validJson: true,
      duplicateTextKinds: [...duplicates],
    };
  } catch {
    return { blockPresent: true, outputPresent: true, validJson: false, duplicateTextKinds: [] };
  }
})()`);
const rawRunDetailsState = await evaluate(`(async () => {
  const inspector = document.querySelector('[aria-label="Run details"]');
  const tabs = [...(inspector?.querySelectorAll('[role="tab"]') ?? [])];
  const rawTab = tabs.find((element) => element.textContent?.trim() === 'Raw JSON');
  rawTab?.click();
  await new Promise((resolveFrame) => requestAnimationFrame(() => requestAnimationFrame(resolveFrame)));
  const raw = inspector?.querySelector('.run-details-json');
  const result = {
    tabPresent: rawTab !== undefined,
    jsonPresent: raw !== null,
    scoped: raw?.textContent?.includes('"scope"') === true,
  };
  const detailsTab = tabs.find((element) => element.textContent?.trim() === 'Details');
  detailsTab?.click();
  await new Promise((resolveFrame) => requestAnimationFrame(resolveFrame));
  return result;
})()`);
const layoutState = await evaluate(`(() => {
  const viewportHeight = document.documentElement.clientHeight;
  const rect = (selector) => {
    const element = document.querySelector(selector);
    if (element === null) return null;
    const bounds = element.getBoundingClientRect();
    return { top: bounds.top, bottom: bounds.bottom, height: bounds.height };
  };
  const scrollState = (selector) => {
    const element = document.querySelector(selector);
    if (element === null) return null;
    return {
      clientHeight: element.clientHeight,
      scrollHeight: element.scrollHeight,
      overflowY: getComputedStyle(element).overflowY,
    };
  };
  const composer = rect('[aria-label="Chat composer"]');
  return {
    viewportHeight,
    bodyScrollHeight: document.body.scrollHeight,
    shell: rect('.desktop-shell'),
    chat: rect('.chat-layout'),
    composer,
    composerFullyVisible:
      composer !== null && composer.top >= 0 && composer.bottom <= viewportHeight + 1,
    timeline: scrollState('.timeline-scroll'),
    inspector: rect('[aria-label="Run details"]'),
    runDetails: scrollState('.run-details-content'),
  };
})()`);
state = {
  ...state,
  runDetailsState,
  modelCallOutputState,
  rawRunDetailsState,
  layoutState,
};

const screenshot = await command("Page.captureScreenshot", {
  format: "png",
  fromSurface: true,
});
await mkdir(dirname(screenshotPath), { recursive: true });
await writeFile(screenshotPath, Buffer.from(screenshot.data, "base64"));
socket.close();

const failures = [];
if (state.title !== "Aworkit") failures.push(`unexpected title: ${state.title}`);
if (state.projectionUnavailable) failures.push("Chat projection is unavailable");
if (state.obsoleteSnapshotError) failures.push("obsolete Chat snapshot fields are required");
if (!state.composerPresent) failures.push("Chat composer was not rendered");
if (!state.composerEnabled) failures.push("Chat composer is disabled");
if (!state.newChatPresent) failures.push("New Chat control was not rendered");
if (!state.runDetailsState.present) failures.push("Run details was not rendered");
if (!state.runDetailsState.entireRunPresent)
  failures.push("whole-run details were not rendered");
if (!state.runDetailsState.executionLogPresent)
  failures.push("Run details has no execution log");
if (state.runDetailsState.detailsContainsRawJson)
  failures.push("Details contains a raw JSON block");
if (state.modelCallOutputState.outputPresent && !state.modelCallOutputState.validJson)
  failures.push("Model-call Output is not valid formatted JSON");
if (state.modelCallOutputState.duplicateTextKinds.length > 0)
  failures.push(
    `Model-call Output still contains streamed fragments for: ${state.modelCallOutputState.duplicateTextKinds.join(", ")}`,
  );
if (!state.rawRunDetailsState.tabPresent)
  failures.push("Raw JSON tab was not rendered");
if (!state.rawRunDetailsState.jsonPresent || !state.rawRunDetailsState.scoped)
  failures.push("Raw JSON does not contain scoped Run details");
const boundedBottom = (bounds) =>
  bounds !== null && bounds.bottom <= state.layoutState.viewportHeight + 1;
if (!boundedBottom(state.layoutState.shell))
  failures.push("desktop shell extends below the WebView");
if (!boundedBottom(state.layoutState.chat))
  failures.push("Chat extends below the WebView");
if (!state.layoutState.composerFullyVisible)
  failures.push("Chat composer is clipped by the WebView");
if (!boundedBottom(state.layoutState.inspector))
  failures.push("Run details extends below the WebView");
if (!["auto", "scroll"].includes(state.layoutState.timeline?.overflowY))
  failures.push("Chat timeline has no independent vertical scroll contract");
if (!["auto", "scroll"].includes(state.layoutState.runDetails?.overflowY))
  failures.push("Run details has no independent vertical scroll contract");

console.log(
  JSON.stringify(
    { ok: failures.length === 0, failures, screenshotPath, state },
    null,
    2,
  ),
);
if (failures.length > 0) process.exitCode = 1;
