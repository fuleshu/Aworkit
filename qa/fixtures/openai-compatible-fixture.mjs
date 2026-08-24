#!/usr/bin/env node

import { appendFile, writeFile } from "node:fs/promises";
import { createServer } from "node:http";

const options = parseArguments(process.argv.slice(2));
const allowUnauthenticated = options.allowUnauthenticated === "true";
const expectedApiKey = options.apiKey ?? "aworkit-rescue-key";
const modelId = options.model ?? "aworkit-rescue-model";
const toolCallMode = options.toolCallMode ?? "none";
const toolPath = options.toolPath ?? "notes.txt";
const toolPrompt = options.toolPrompt ?? `Read ${toolPath} from the selected project.`;
const followupPrompt =
  options.followupPrompt ?? "Confirm after restart without reading the file again.";
const expectedToolContent = options.expectedToolContent ?? "";
const readyFile = required(options, "readyFile");
const requestLog = required(options, "requestLog");
const maximumBodyBytes = 1024 * 1024;
let completionCount = 0;
let settledToolContent = null;

if (!new Set(["none", "read-project-file"]).has(toolCallMode)) {
  throw new Error(`unsupported --tool-call-mode ${toolCallMode}`);
}

await writeFile(requestLog, "", { mode: 0o600 });

const server = createServer(async (request, response) => {
  try {
    const url = new URL(request.url ?? "/", "http://127.0.0.1");
    if (request.method === "GET" && url.pathname === "/healthz") {
      return json(response, 200, { status: "ready" });
    }
    if (request.method === "GET" && url.pathname === "/v1/models") {
      if (!authorized(request)) return unauthorized(response);
      await record({ kind: "models", method: request.method, path: url.pathname });
      return json(response, 200, {
        object: "list",
        data: [
          {
            id: modelId,
            object: "model",
            created: 1_700_000_000,
            owned_by: "aworkit-rescue-fixture",
          },
        ],
      });
    }
    if (request.method === "POST" && url.pathname === "/v1/chat/completions") {
      if (!authorized(request)) return unauthorized(response);
      const body = await readJsonBody(request);
      if (toolCallMode === "read-project-file") {
        return toolCompletion(response, body);
      }
      validateCompletion(body);
      completionCount += 1;
      const prompt = lastUserText(body.messages);
      const reply = `AWORKIT_FIXTURE_REPLY_${completionCount}: ${prompt}`;
      await record({
        kind: "chat.completion",
        method: request.method,
        path: url.pathname,
        model: body.model,
        stream: body.stream === true,
        messages: body.messages,
        reply,
      });
      if (body.stream === true) return streamCompletion(response, reply);
      return json(response, 200, completionEnvelope(reply));
    }
    if (request.method === "GET" && url.pathname === "/__fixture/requests") {
      return json(response, 200, { requestLog });
    }
    return json(response, 404, {
      error: { message: `fixture route not found: ${request.method} ${url.pathname}` },
    });
  } catch (error) {
    return json(response, error.statusCode ?? 500, {
      error: { message: error instanceof Error ? error.message : String(error) },
    });
  }
});

server.on("clientError", (_error, socket) => {
  socket.end("HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n");
});

await new Promise((resolve, reject) => {
  server.once("error", reject);
  server.listen(0, "127.0.0.1", resolve);
});
const address = server.address();
if (address === null || typeof address === "string") {
  throw new Error("fixture did not bind an IPv4 TCP address");
}
const ready = {
  schemaVersion: 1,
  baseUrl: `http://127.0.0.1:${address.port}/v1`,
  healthUrl: `http://127.0.0.1:${address.port}/healthz`,
  apiKey: expectedApiKey,
  model: modelId,
  toolCallMode,
  pid: process.pid,
};
await writeFile(readyFile, `${JSON.stringify(ready)}\n`, { mode: 0o600 });
process.stdout.write(`${JSON.stringify(ready)}\n`);

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => {
    server.close(() => process.exit(0));
    setTimeout(() => process.exit(1), 2_000).unref();
  });
}

function authorized(request) {
  return (
    allowUnauthenticated ||
    request.headers.authorization === `Bearer ${expectedApiKey}`
  );
}

function unauthorized(response) {
  return json(response, 401, {
    error: { message: "invalid rescue-fixture API key", type: "authentication_error" },
  });
}

function completionEnvelope(reply) {
  return {
    id: `chatcmpl-aworkit-${completionCount}`,
    object: "chat.completion",
    created: 1_700_000_000 + completionCount,
    model: modelId,
    choices: [
      {
        index: 0,
        message: { role: "assistant", content: reply },
        finish_reason: "stop",
      },
    ],
    usage: { prompt_tokens: 7, completion_tokens: 9, total_tokens: 16 },
  };
}

async function toolCompletion(response, body) {
  // Record every arrival before validating it. A duplicated request caused by
  // replay must remain visible to the native gate even when the fixture rejects it.
  await record({
    kind: "chat.completion",
    fixtureMode: toolCallMode,
    method: "POST",
    path: "/v1/chat/completions",
    model: body?.model,
    stream: body?.stream === true,
    messages: body?.messages,
    tools: body?.tools,
    toolChoice: body?.tool_choice,
  });
  const phase = validateToolCompletion(body);
  completionCount += 1;
  if (phase.kind === "request_read") {
    return json(response, 200, toolCallEnvelope("call_read_1", toolPath));
  }
  if (phase.kind === "settle_read") {
    settledToolContent = phase.toolContent;
    return json(response, 200, completionEnvelope(toolFinalReply(settledToolContent)));
  }
  return json(response, 200, completionEnvelope(followupReply()));
}

function validateToolCompletion(body) {
  validateCompletionShape(body);
  if (body.stream !== false || body.tool_choice !== "auto") {
    badRequest("tool fixture requires a non-streaming automatic tool request");
  }
  if (!Array.isArray(body.tools) || body.tools.length !== 1) {
    badRequest("tool fixture requires exactly one frozen tool definition");
  }
  const [tool] = body.tools;
  if (
    tool?.type !== "function" ||
    tool.function?.name !== "aworkit_read_project_file" ||
    typeof tool.function.description !== "string" ||
    tool.function.description.trim() === "" ||
    tool.function.parameters?.type !== "object"
  ) {
    badRequest("tool fixture received the wrong read-tool definition");
  }

  const messages = body.messages;
  const latestUser = [...messages]
    .reverse()
    .find((candidate) => candidate?.role === "user");
  if (latestUser?.content === followupPrompt) {
    if (
      settledToolContent === null ||
      messages.length !== 3 ||
      !plainMessage(messages[0], "user", toolPrompt) ||
      !plainMessage(messages[1], "assistant", toolFinalReply(settledToolContent)) ||
      !plainMessage(messages[2], "user", followupPrompt)
    ) {
      badRequest("post-restart follow-up did not contain the exact settled conversation");
    }
    return { kind: "follow_up" };
  }
  if (latestUser?.content !== toolPrompt) {
    badRequest("tool fixture received an unexpected user prompt");
  }
  if (messages.length === 1 && plainMessage(messages[0], "user", toolPrompt)) {
    if (settledToolContent !== null) {
      badRequest("the initial provider/tool request was replayed after settlement");
    }
    return { kind: "request_read" };
  }
  if (
    messages.length !== 3 ||
    !plainMessage(messages[0], "user", toolPrompt) ||
    messages[1]?.role !== "assistant" ||
    messages[1]?.content !== null ||
    !Array.isArray(messages[1]?.tool_calls) ||
    messages[1].tool_calls.length !== 1 ||
    messages[2]?.role !== "tool" ||
    messages[2]?.tool_call_id !== "call_read_1" ||
    typeof messages[2]?.content !== "string"
  ) {
    badRequest("tool result turn did not preserve the exact OpenAI correlation shape");
  }
  const call = messages[1].tool_calls[0];
  if (
    call?.id !== "call_read_1" ||
    call?.type !== "function" ||
    call.function?.name !== "aworkit_read_project_file" ||
    call.function?.arguments !== JSON.stringify({ path: toolPath })
  ) {
    badRequest("tool result turn changed the frozen read request");
  }
  let result;
  try {
    result = JSON.parse(messages[2].content);
  } catch {
    badRequest("read-tool result must be one JSON object");
  }
  if (
    result === null ||
    typeof result !== "object" ||
    Array.isArray(result) ||
    result.path !== toolPath ||
    typeof result.content !== "string" ||
    result.content.length === 0 ||
    typeof result.contentHash !== "string" ||
    !/^sha256:[0-9a-f]{64}$/u.test(result.contentHash) ||
    result.bytes !== Buffer.byteLength(result.content, "utf8") ||
    (expectedToolContent !== "" && !result.content.includes(expectedToolContent))
  ) {
    badRequest("tool result did not prove the expected bounded project-file read");
  }
  return { kind: "settle_read", toolContent: result.content };
}

function validateCompletionShape(body) {
  if (body === null || typeof body !== "object" || Array.isArray(body)) {
    badRequest("completion body must be an object");
  }
  if (body.model !== modelId) badRequest(`expected model ${modelId}`);
  if (!Array.isArray(body.messages) || body.messages.length === 0) {
    badRequest("messages must be a non-empty array");
  }
}

function plainMessage(message, role, content) {
  return (
    message !== null &&
    typeof message === "object" &&
    !Array.isArray(message) &&
    Object.keys(message).length === 2 &&
    message.role === role &&
    message.content === content
  );
}

function toolCallEnvelope(callId, path) {
  return {
    id: `chatcmpl-aworkit-${completionCount}`,
    object: "chat.completion",
    created: 1_700_000_000 + completionCount,
    model: modelId,
    choices: [
      {
        index: 0,
        message: {
          role: "assistant",
          content: null,
          tool_calls: [
            {
              id: callId,
              type: "function",
              function: {
                name: "aworkit_read_project_file",
                arguments: JSON.stringify({ path }),
              },
            },
          ],
        },
        finish_reason: "tool_calls",
      },
    ],
    usage: { prompt_tokens: 11, completion_tokens: 5, total_tokens: 16 },
  };
}

function toolFinalReply(toolContent) {
  return `AWORKIT_TOOL_FINAL: ${toolContent}`;
}

function followupReply() {
  return "AWORKIT_TOOL_FOLLOWUP: settled context resumed without another tool call";
}

function streamCompletion(response, reply) {
  response.writeHead(200, {
    "content-type": "text/event-stream; charset=utf-8",
    "cache-control": "no-cache",
    connection: "close",
  });
  const midpoint = Math.max(1, Math.floor(reply.length / 2));
  const chunks = [reply.slice(0, midpoint), reply.slice(midpoint)];
  for (const [index, content] of chunks.entries()) {
    response.write(
      `data: ${JSON.stringify({
        id: `chatcmpl-aworkit-${completionCount}`,
        object: "chat.completion.chunk",
        created: 1_700_000_000 + completionCount,
        model: modelId,
        choices: [
          {
            index: 0,
            delta: index === 0 ? { role: "assistant", content } : { content },
            finish_reason: null,
          },
        ],
      })}\n\n`,
    );
  }
  response.write(
    `data: ${JSON.stringify({
      id: `chatcmpl-aworkit-${completionCount}`,
      object: "chat.completion.chunk",
      created: 1_700_000_000 + completionCount,
      model: modelId,
      choices: [{ index: 0, delta: {}, finish_reason: "stop" }],
      usage: { prompt_tokens: 7, completion_tokens: 9, total_tokens: 16 },
    })}\n\n`,
  );
  response.end("data: [DONE]\n\n");
}

async function record(value) {
  await appendFile(requestLog, `${JSON.stringify(value)}\n`, { mode: 0o600 });
}

async function readJsonBody(request) {
  const chunks = [];
  let size = 0;
  for await (const chunk of request) {
    size += chunk.length;
    if (size > maximumBodyBytes) {
      const error = new Error("request body exceeds one MiB");
      error.statusCode = 413;
      throw error;
    }
    chunks.push(chunk);
  }
  try {
    return JSON.parse(Buffer.concat(chunks).toString("utf8"));
  } catch {
    const error = new Error("request body must be valid JSON");
    error.statusCode = 400;
    throw error;
  }
}

function validateCompletion(body) {
  validateCompletionShape(body);
  for (const message of body.messages) {
    if (
      message === null ||
      typeof message !== "object" ||
      !["system", "user", "assistant", "tool"].includes(message.role) ||
      typeof message.content !== "string"
    ) {
      badRequest("every fixture message requires a supported role and string content");
    }
  }
  lastUserText(body.messages);
}

function lastUserText(messages) {
  const message = [...messages].reverse().find((candidate) => candidate.role === "user");
  if (message === undefined || message.content.trim() === "") {
    badRequest("at least one non-empty user message is required");
  }
  return message.content;
}

function badRequest(message) {
  const error = new Error(message);
  error.statusCode = 400;
  throw error;
}

function json(response, status, value) {
  const body = `${JSON.stringify(value)}\n`;
  response.writeHead(status, {
    "content-type": "application/json; charset=utf-8",
    "content-length": Buffer.byteLength(body),
    connection: "close",
  });
  response.end(body);
}

function required(values, name) {
  const value = values[name];
  if (value === undefined || value === "") throw new Error(`--${kebab(name)} is required`);
  return value;
}

function parseArguments(arguments_) {
  const result = {};
  for (let index = 0; index < arguments_.length; index += 2) {
    const name = arguments_[index];
    const value = arguments_[index + 1];
    if (!name?.startsWith("--") || value === undefined) {
      throw new Error(`invalid fixture arguments near ${name ?? "<end>"}`);
    }
    result[camel(name.slice(2))] = value;
  }
  return result;
}

function camel(value) {
  return value.replaceAll(/-([a-z])/g, (_match, character) => character.toUpperCase());
}

function kebab(value) {
  return value.replaceAll(/[A-Z]/g, (character) => `-${character.toLowerCase()}`);
}
