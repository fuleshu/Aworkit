/**
 * Presentation model for the Chat tool-call card.
 *
 * These helpers turn one immutable timeline item into the three things the card
 * shows: the real tool behind a capability id, one formatted summary of the
 * arguments the model actually passed, and the tool's own output. They are pure
 * and read-only — ordering, status derivation, attribution and the projection
 * contract itself stay exactly as the semantic projection defines them.
 */
import type { ImageAttachment } from "./images";
import type { TimelineItem } from "./types";

type FactPayload = Record<string, unknown>;

/**
 * Field caps in code rows. The card's stylesheet multiplies a cap by the current
 * code line height (which follows OS text size and application scaling), so the
 * cap is never expressed in fixed pixels.
 */
export const TOOL_INPUT_COLLAPSED_ROWS = 1;
export const TOOL_INPUT_MAX_ROWS = 10;
export const TOOL_OUTPUT_ROWS = 4;
export const TOOL_OUTPUT_EXPANDED_ROWS = 10;

export type ShellDialect = "bash" | "powershell";

/**
 * The concrete tool a reader should picture behind a bundled capability. An id
 * that is not listed — an MCP server tool, or a capability added later — keeps
 * its own id and is never given an invented name.
 */
const CAPABILITY_DISPLAY_NAMES: Readonly<Record<string, string>> = {
  "tool.files.read": "File read",
  "tool.files.search": "File search",
  "tool.files.edit": "File edit",
  "tool.files.list": "File list",
  "tool.files.grep": "File regex search",
  "tool.files.write": "File write",
  "tool.python.host": "Host Python",
  "tool.python.start": "Python job",
  "tool.todo": "Run task list",
  "tool.goal": "Chat goal",
  "tool.ask_user": "Ask the user",
  "tool.browse": "File or folder picker",
  "tool.web_search": "Web search",
  "tool.web_fetch": "Web page fetch",
  "tool.web_extract": "Web page extract",
  "tool.subagent": "Subagent delegation",
  "tool.subagent_codex": "Codex agent",
  "tool.subagent_claude_code": "Claude Code agent",
  "tool.subagent_fork": "Forked subagent",
  "tool.subagent_list": "List subagents",
  "tool.subagent_message": "Message subagent",
  "tool.subagent_cancel": "Cancel subagent",
  "tool.skill": "Skills",
  "tool.workspace_instructions": "Workspace instructions",
  "tool.context": "Context retrieval",
  "tool.image.read": "Image read",
  "tool.screenshot": "Screenshot",
  "tool.job.output": "Read job output",
  "tool.job.input": "Send job input",
  "tool.job.stop": "Stop job",
  "tool.job.list": "List jobs",
  "tool.job.keep": "Keep job running",
};

/** Capabilities whose real tool is the host shell dialect, not a product name. */
const SHELL_CAPABILITIES: ReadonlySet<string> = new Set([
  "tool.shell.host",
  "tool.shell.start",
]);

/** Capabilities whose call acts on one workspace path the card can offer actions for. */
const FILE_PATH_CAPABILITIES: ReadonlySet<string> = new Set([
  "tool.files.read",
  "tool.files.write",
  "tool.files.edit",
  "tool.files.list",
  "tool.files.search",
  "tool.files.grep",
]);

/**
 * Provider correlation the model never passed as an argument. It is exactly the
 * envelope the runtime records around a call, so it is dropped only when the
 * input is recognisably that envelope.
 */
const CALL_ENVELOPE_KEYS: ReadonlySet<string> = new Set([
  "callId",
  "providerCallId",
  "capabilityId",
  "name",
]);

/** Result keys the capability host uses for text, in reading order. */
const RESULT_TEXT_KEYS = [
  "text",
  "markdown",
  "snippet",
  "body",
  "output",
  "stdout",
  "stderr",
  "message",
  "error",
  "answer",
  "summary",
  "value",
] as const;

/** Result keys whose value is a list of text lines. */
const RESULT_LIST_KEYS = [
  "files",
  "entries",
  "matches",
  "results",
  "items",
  "paths",
  "lines",
  "urls",
] as const;

/** Bounds a pathological result tree without ever hiding a top-level text key. */
const MAX_EXTRACTION_DEPTH = 4;

/**
 * The dialect of the shell the host capability would actually launch. This uses
 * the same runtime identity rule as the Settings shell-command validation.
 */
export function hostShellDialect(): ShellDialect {
  const identity =
    typeof navigator === "undefined"
      ? ""
      : `${navigator.userAgent} ${navigator.platform}`;
  return /(?:windows|win32|win64|wow64)/iu.test(identity)
    ? "powershell"
    : "bash";
}

/** The tool entry a capability id names, or undefined when nothing names it. */
export function toolDisplayName(
  capabilityId: unknown,
  dialect: ShellDialect = hostShellDialect(),
): string | undefined {
  if (typeof capabilityId !== "string" || capabilityId.length === 0)
    return undefined;
  if (SHELL_CAPABILITIES.has(capabilityId)) return dialect;
  return CAPABILITY_DISPLAY_NAMES[capabilityId];
}

/** The capability id a tool activity carries, when the span recorded one. */
export function toolCapabilityId(item: TimelineItem): string | undefined {
  const capabilityId = record(item.metadata).capabilityId;
  return typeof capabilityId === "string" && capabilityId.length > 0
    ? capabilityId
    : undefined;
}

/**
 * The card header: the capability id, followed by the real tool in brackets.
 * An id nothing names degrades to the id alone — never empty brackets.
 */
export function toolHeaderLabel(
  item: TimelineItem,
  dialect: ShellDialect = hostShellDialect(),
): string {
  const capabilityId = toolCapabilityId(item) ?? item.title;
  const name = toolDisplayName(capabilityId, dialect);
  return name === undefined ? capabilityId : `${capabilityId} (${name})`;
}

/** The input a tool activity recorded, whether on the item or its metadata. */
export function toolInputValue(item: TimelineItem): unknown {
  if (item.input !== undefined) return item.input;
  return record(item.metadata).input;
}

/**
 * The arguments the model actually passed. The runtime records a call as
 * `{callId, providerCallId, capabilityId, name, arguments}`, so the arguments
 * are the payload; an item that already carries plain arguments is used as-is.
 */
export function toolArguments(input: unknown): FactPayload | undefined {
  const value = asRecord(input);
  if (value === undefined) return undefined;
  const declared = asRecord(value.arguments);
  if (declared !== undefined) return declared;
  if (!isCallEnvelope(value)) return value;
  const own: FactPayload = {};
  for (const [key, entry] of Object.entries(value)) {
    if (key === "arguments" || CALL_ENVELOPE_KEYS.has(key)) continue;
    own[key] = entry;
  }
  return own;
}

/** One formatted argument: the key it came from, its label, and its value. */
export interface ToolArgumentSegment {
  readonly key: string;
  readonly label: string;
  readonly text: string;
}

/** One formatted line per argument, shaped by what the argument is. */
export function toolArgumentSegments(
  input: unknown,
): readonly ToolArgumentSegment[] {
  const arguments_ = toolArguments(input);
  if (arguments_ === undefined) return [];
  const segments: ToolArgumentSegment[] = [];
  for (const [key, value] of Object.entries(arguments_)) {
    const text = argumentText(value);
    if (text === undefined) continue;
    segments.push({ key, label: argumentLabel(key), text });
  }
  return segments;
}

/** The formatted argument line, exactly as the card renders it. */
export function toolInputText(
  segments: readonly ToolArgumentSegment[],
): string {
  return segments
    .map((segment) => `${segment.label}: ${segment.text}`)
    .join(" · ");
}

/** True when an argument value carries its own line breaks. */
export function toolInputIsMultiline(
  segments: readonly ToolArgumentSegment[],
): boolean {
  return segments.some((segment) => segment.text.includes("\n"));
}

/**
 * The workspace path one file tool acted on, exactly as the model passed it.
 * Only the tools whose only location argument is a path qualify, so the card
 * never offers an action for a pattern, a command or a web address.
 */
export function fileToolPath(item: TimelineItem): string | undefined {
  if (item.kind !== "tool") return undefined;
  const capabilityId = toolCapabilityId(item);
  if (capabilityId === undefined || !FILE_PATH_CAPABILITIES.has(capabilityId))
    return undefined;
  const arguments_ = toolArguments(toolInputValue(item));
  const path = arguments_?.path;
  return typeof path === "string" && path.length > 0 && path.length <= 4_096
    ? path
    : undefined;
}

/**
 * The output the card shows: the tool's own text (streaming while it runs), a
 * previewable image, or a compact status for a result that has no text at all.
 */
export type ToolOutputView =
  | {
      readonly kind: "text";
      readonly text: string;
      readonly streaming: boolean;
    }
  | { readonly kind: "image"; readonly images: readonly ImageAttachment[] }
  | { readonly kind: "status"; readonly text: string };

export function toolOutputView(item: TimelineItem): ToolOutputView | undefined {
  const images = item.attachments ?? [];
  if (images.length > 0) return { kind: "image", images };
  const metadata = record(item.metadata);
  const channels = record(metadata.channels);
  const progress = typeof channels.progress === "string" ? channels.progress : "";
  const output = item.output !== undefined ? item.output : metadata.output;
  const busy = isToolBusy(item.status);
  if (busy && progress.trim().length > 0)
    return { kind: "text", text: progress, streaming: true };
  if (output !== undefined) {
    const text = toolResultText(output);
    if (text !== undefined) return { kind: "text", text, streaming: false };
    return { kind: "status", text: toolResultStatus(output) };
  }
  const fallback = progress.trim().length > 0 ? progress : (item.body ?? "");
  if (fallback.trim().length === 0) return undefined;
  return { kind: "text", text: fallback, streaming: busy };
}

/**
 * The tool's own text inside a result value, whatever the capability returned.
 * Recognised text keys, their lists and pre-canonical fragments are joined;
 * anything else yields undefined so the card shows a status instead of a blob.
 */
export function toolResultText(
  value: unknown,
  depth = 0,
): string | undefined {
  if (depth > MAX_EXTRACTION_DEPTH) return undefined;
  if (typeof value === "string")
    return value.trim().length > 0 ? value : undefined;
  if (typeof value === "number" || typeof value === "boolean")
    return String(value);
  if (Array.isArray(value)) {
    const parts = value
      .map((entry) => toolResultText(entry, depth + 1))
      .filter((part): part is string => part !== undefined);
    return parts.length > 0 ? parts.join("\n") : undefined;
  }
  if (!isRecord(value)) return undefined;
  // The canonical tool result wraps the payload in `content`.
  if ("content" in value) {
    const content = toolResultText(value.content, depth + 1);
    if (content !== undefined) return content;
  }
  const parts: string[] = [];
  for (const key of [...RESULT_TEXT_KEYS, ...RESULT_LIST_KEYS]) {
    if (!(key in value)) continue;
    const part = toolResultText(value[key], depth + 1);
    if (part !== undefined) parts.push(part);
  }
  return parts.length > 0 ? parts.join("\n") : undefined;
}

/** A short, human status for a result that carries no text to render. */
export function toolResultStatus(value: unknown): string {
  if (isFailedResult(value)) return "Failed with no text output";
  const payload = unwrapResultContent(value);
  if (isFailedResult(payload)) return "Failed with no text output";
  if (payload === null || payload === undefined) return "No output";
  if (typeof payload === "string") return "Empty text output";
  if (Array.isArray(payload))
    return payload.length === 0 ? "Empty list" : `List of ${payload.length} items`;
  if (isRecord(payload)) {
    const exitCode = payload.exitCode;
    if (typeof exitCode === "number")
      return exitCode === 0
        ? "No output (exit code 0)"
        : `No output (exit code ${exitCode})`;
    const fields = Object.keys(payload).length;
    if (fields === 0) return "Empty result";
    return `Structured result (${fields} ${fields === 1 ? "field" : "fields"})`;
  }
  return String(payload);
}

/** True while a tool call has not reached a terminal lifecycle fact. */
export function isToolBusy(status: string | undefined): boolean {
  return (
    status === "running" ||
    status === "started" ||
    status === "queued" ||
    status === "waiting"
  );
}

function argumentText(value: unknown): string | undefined {
  if (typeof value === "string") {
    const trimmed = value.trim();
    return trimmed.length > 0 ? trimmed : undefined;
  }
  if (typeof value === "number" || typeof value === "boolean")
    return String(value);
  if (Array.isArray(value)) {
    if (value.length === 0) return undefined;
    if (
      value.every(
        (entry) =>
          typeof entry === "string" ||
          typeof entry === "number" ||
          typeof entry === "boolean",
      )
    )
      return value.map(String).join(", ");
    return `${value.length} ${value.length === 1 ? "item" : "items"}`;
  }
  if (isRecord(value)) {
    const fields = Object.keys(value).length;
    return fields === 0 ? undefined : `${fields} ${fields === 1 ? "field" : "fields"}`;
  }
  return undefined;
}

/** Turns an argument key into the words the card shows in front of its value. */
function argumentLabel(key: string): string {
  const words = key
    .replaceAll(/([a-z0-9])([A-Z])/gu, "$1 $2")
    .replaceAll("_", " ")
    .trim()
    .toLowerCase();
  return words.length > 0 ? words : key;
}

function unwrapResultContent(value: unknown): unknown {
  if (!isRecord(value)) return value;
  return "content" in value ? value.content : value;
}

function isFailedResult(value: unknown): boolean {
  return isRecord(value) && value.isError === true;
}

function isCallEnvelope(value: FactPayload): boolean {
  return typeof value.callId === "string" && typeof value.capabilityId === "string";
}

function asRecord(value: unknown): FactPayload | undefined {
  if (isRecord(value)) return value;
  if (typeof value !== "string") return undefined;
  try {
    const parsed: unknown = JSON.parse(value);
    return isRecord(parsed) ? parsed : undefined;
  } catch {
    return undefined;
  }
}

function record(value: unknown): FactPayload {
  return isRecord(value) ? value : {};
}

function isRecord(value: unknown): value is FactPayload {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
