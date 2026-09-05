// Isolated native WebView fixture for manual/automated picker and OS-paste QA.
// Run after npm run build and cargo build. Prints image paths and records the
// exact image count reaching the real OpenAI-compatible HTTP adapter.
import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { deflateSync } from "node:zlib";
import { once } from "node:events";

const root = resolve(`src-tauri/target/native-vision-${Date.now()}`);
await mkdir(root, { recursive: true });
function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit++)
      crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0);
  }
  return (crc ^ 0xffffffff) >>> 0;
}
function chunk(type, data) {
  const body = Buffer.concat([Buffer.from(type), data]);
  const size = Buffer.alloc(4);
  size.writeUInt32BE(data.length);
  const checksum = Buffer.alloc(4);
  checksum.writeUInt32BE(crc32(body));
  return Buffer.concat([size, body, checksum]);
}
function png(color) {
  const header = Buffer.alloc(13);
  header.writeUInt32BE(320);
  header.writeUInt32BE(200, 4);
  header[8] = 8;
  header[9] = 2;
  const pixels = Buffer.alloc(200 * (1 + 320 * 3));
  for (let y = 0; y < 200; y++)
    for (let x = 0; x < 320; x++)
      for (let c = 0; c < 3; c++) pixels[y * 961 + 1 + x * 3 + c] = color[c];
  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk("IHDR", header),
    chunk("IDAT", deflateSync(pixels)),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}
const paths = [resolve(root, "red.png"), resolve(root, "blue.png")];
await writeFile(paths[0], png([230, 55, 65]));
await writeFile(paths[1], png([35, 115, 220]));
const requests = [];
const server = createServer(async (request, response) => {
  response.setHeader("Content-Type", "application/json");
  if (request.url === "/v1/models")
    return response.end(
      JSON.stringify({
        data: [{ id: "vision-fixture", capabilities: { vision: true } }],
      }),
    );
  const chunks = [];
  for await (const chunk of request) chunks.push(chunk);
  const body = JSON.parse(Buffer.concat(chunks).toString());
  const parts = body.messages.flatMap((message) =>
    Array.isArray(message.content) ? message.content : [],
  );
  const images = parts.filter((part) => part.type === "image_url");
  if (
    !images.length ||
    images.some(
      (image) =>
        !/^data:image\/(png|jpeg|webp);base64,/.test(image.image_url?.url),
    )
  ) {
    response.statusCode = 400;
    return response.end(
      JSON.stringify({ error: "Native vision fixture expected image content" }),
    );
  }
  requests.push({
    imageCount: images.length,
    byteLengths: images.map(
      (image) =>
        Buffer.from(image.image_url.url.split(",")[1], "base64").length,
    ),
    text: body.messages
      .at(-1)
      .content.filter((part) => part.type === "text")
      .map((part) => part.text)
      .join("\n"),
  });
  await writeFile(
    resolve(root, "wire-report.json"),
    JSON.stringify(requests, null, 2),
  );
  console.log("PROVIDER", JSON.stringify(requests.at(-1)));
  response.setHeader("Content-Type", "text/event-stream");
  const text = `Vision fixture received ${images.length} image(s) through the native provider adapter.`;
  response.end(
    `data: ${JSON.stringify({ choices: [{ index: 0, delta: { content: text }, finish_reason: null }] })}\n\ndata: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: "stop" }], usage: { prompt_tokens: 100, completion_tokens: 16 } })}\n\ndata: [DONE]\n\n`,
  );
});
server.listen(0, "127.0.0.1");
await once(server, "listening");
const origin = `http://127.0.0.1:${server.address().port}`;
const child = spawn(resolve("src-tauri/target/debug/aworkit-desktop.exe"), [], {
  windowsHide: true,
  env: {
    ...process.env,
    AWORKIT_QA_PROFILE: root,
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: "--remote-debugging-port=9223",
  },
  stdio: "ignore",
});
child.on("exit", () => server.close());
let target;
for (let attempt = 0; attempt < 60; attempt++) {
  try {
    target = (
      await fetch("http://127.0.0.1:9223/json/list").then((r) => r.json())
    ).find((t) => t.url.startsWith("http://tauri.localhost"));
  } catch {}
  if (target) break;
  await new Promise((resolve) => setTimeout(resolve, 250));
}
if (!target) throw new Error("Native WebView did not start");
const socket = new WebSocket(target.webSocketDebuggerUrl);
await once(socket, "open");
let id = 0;
const pending = new Map();
socket.addEventListener("message", ({ data }) => {
  const result = JSON.parse(data);
  if (!result.id) return;
  const call = pending.get(result.id);
  pending.delete(result.id);
  if (result.error) call.reject(result.error);
  else call.resolve(result.result);
});
function command(method, params) {
  return new Promise((resolve, reject) => {
    const next = ++id;
    pending.set(next, { resolve, reject });
    socket.send(JSON.stringify({ id: next, method, params }));
  });
}
async function evaluate(expression) {
  const result = await command("Runtime.evaluate", {
    expression,
    awaitPromise: true,
    returnByValue: true,
  });
  if (result.exceptionDetails)
    throw new Error(JSON.stringify(result.exceptionDetails));
  return result.result.value;
}
for (let attempt = 0; attempt < 60; attempt++) {
  if (await evaluate("Boolean(window.__TAURI_INTERNALS__?.invoke)")) break;
  await new Promise((resolve) => setTimeout(resolve, 250));
}
await evaluate(`(async () => {
  const invoke = window.__TAURI_INTERNALS__.invoke;
  const settings = await invoke('settings_snapshot');
  await invoke('settings_commit', { command: { commandId: 'native.vision.configure', expectedVersion: settings.version, appearance: 'system', portableHistoryEnabled: false, provider: { baseUrl: '${origin}/v1', model: 'vision-fixture', credentialAction: 'keep', apiKey: null } } });
  const v2 = await invoke('settings_v2_snapshot');
  for (const provider of v2.settings.providers) for (const model of provider.models) model.capabilities = [...new Set([...model.capabilities, 'vision'])];
  await invoke('settings_v2_commit', { command: { commandId: 'native.vision.enable', expectedVersion: v2.version, settings: v2.settings } });
})()`);
await command("Page.reload", {});
socket.close();
await writeFile(
  resolve(root, "fixture.json"),
  JSON.stringify({ root, paths, origin, pid: child.pid }, null, 2),
);
console.log("READY", JSON.stringify({ root, paths, origin, pid: child.pid }));
