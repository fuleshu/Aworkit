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

console.log(
  JSON.stringify(
    { ok: failures.length === 0, failures, screenshotPath, state },
    null,
    2,
  ),
);
if (failures.length > 0) process.exitCode = 1;
