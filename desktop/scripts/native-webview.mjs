import { writeFile } from "node:fs/promises";

/** Bounded connection to an actual Tauri WebView for native regression tests. */
export async function connectNativeWebView(endpoint, pageUrlPrefix = "http://tauri.localhost") {
  const response = await fetch(`${endpoint}/json/list`);
  if (!response.ok) throw new Error(`WebView discovery failed: HTTP ${response.status}`);
  const pages = (await response.json()).filter(item => item.type === "page");
  const target = pages.find(item => item.url.startsWith(pageUrlPrefix));
  if (!target) throw new Error("No matching Aworkit WebView was found: " + pages.map(item => item.url).join(", "));
  const socket = new WebSocket(target.webSocketDebuggerUrl);
  const pending = new Map();
  let sequence = 0;
  socket.addEventListener("message", ({ data }) => {
    const message = JSON.parse(data), request = pending.get(message.id);
    if (!request) return;
    pending.delete(message.id);
    clearTimeout(request.timer);
    if (message.error) request.reject(new Error(message.error.message));
    else request.resolve(message.result);
  });
  await new Promise((resolve, reject) => {
    socket.addEventListener("open", resolve, { once: true });
    socket.addEventListener("error", reject, { once: true });
  });
  const command = (method, params = {}) => new Promise((resolve, reject) => {
    const id = ++sequence;
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`Timed out: ${method}`)); }, 30_000);
    pending.set(id, { resolve, reject, timer });
    socket.send(JSON.stringify({ id, method, params }));
  });
  return {
    command,
    async evaluate(expression) {
      const result = await command("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
      if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text);
      return result.result.value;
    },
    async screenshot(path) {
      const result = await command("Page.captureScreenshot", { format: "png" });
      await writeFile(path, Buffer.from(result.data, "base64"));
    },
    close() { socket.close(); for (const request of pending.values()) clearTimeout(request.timer); },
  };
}
