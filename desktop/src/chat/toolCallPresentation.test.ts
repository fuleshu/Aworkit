import { describe, expect, it } from "vitest";
import type { TimelineItem } from "./types";
import {
  TOOL_INPUT_COLLAPSED_ROWS,
  TOOL_INPUT_MAX_ROWS,
  TOOL_OUTPUT_EXPANDED_ROWS,
  TOOL_OUTPUT_ROWS,
  fileToolPath,
  hostShellDialect,
  isToolBusy,
  toolArgumentSegments,
  toolArguments,
  toolDisplayName,
  toolHeaderLabel,
  toolInputIsMultiline,
  toolInputText,
  toolOutputView,
  toolResultStatus,
  toolResultText,
} from "./toolCallPresentation";

function item(overrides: Partial<TimelineItem> = {}): TimelineItem {
  return {
    id: "span.tool.1",
    kind: "tool",
    title: "tool.files.read",
    createdAt: "2026-01-01T00:00:00Z",
    status: "completed",
    ...overrides,
  };
}

describe("tool-call header naming", () => {
  it("names the real tool behind a capability id", () => {
    expect(toolDisplayName("tool.files.read")).toBe("File read");
    expect(toolDisplayName("tool.files.grep")).toBe("File regex search");
    expect(toolHeaderLabel(item())).toBe("tool.files.read (File read)");
  });

  it("uses the host shell dialect as the shell tool's own name", () => {
    expect(toolDisplayName("tool.shell.host", "bash")).toBe("bash");
    expect(toolDisplayName("tool.shell.host", "powershell")).toBe("powershell");
    expect(toolDisplayName("tool.shell.start", "powershell")).toBe("powershell");
    expect(toolHeaderLabel(item({ title: "tool.shell.host" }), "powershell")).toBe(
      "tool.shell.host (powershell)",
    );
  });

  it("reads the capability id from the committed metadata, not the title", () => {
    expect(
      toolHeaderLabel(
        item({
          title: "Read notes",
          metadata: { capabilityId: "tool.files.read" },
        }),
      ),
    ).toBe("tool.files.read (File read)");
  });

  it("degrades an unmapped capability id to the id alone", () => {
    expect(toolDisplayName("mcp://server/tool")).toBeUndefined();
    expect(toolDisplayName(undefined)).toBeUndefined();
    expect(toolHeaderLabel(item({ title: "mcp://server/tool" }))).toBe(
      "mcp://server/tool",
    );
    expect(toolHeaderLabel(item({ title: "Read notes" }))).toBe("Read notes");
  });

  it("recognizes only the two shell capabilities as dialect-named", () => {
    expect(hostShellDialect()).toBe("bash");
    expect(toolDisplayName("tool.shell.run", "bash")).toBeUndefined();
  });

  it("keeps the row caps the card applies in a fixed order", () => {
    expect(TOOL_INPUT_COLLAPSED_ROWS).toBeLessThan(TOOL_INPUT_MAX_ROWS);
    expect(TOOL_OUTPUT_ROWS).toBeLessThan(TOOL_OUTPUT_EXPANDED_ROWS);
    expect(isToolBusy("running")).toBe(true);
    expect(isToolBusy("waiting")).toBe(true);
    expect(isToolBusy("completed")).toBe(false);
  });
});

describe("formatted tool arguments", () => {
  it("reads the arguments out of the runtime call envelope", () => {
    expect(
      toolArguments({
        callId: "call.1",
        providerCallId: "call.1",
        capabilityId: "tool.shell.host",
        name: "shell",
        arguments: { command: "ls -la" },
      }),
    ).toEqual({ command: "ls -la" });
  });

  it("keeps plain arguments and drops the envelope without inventing any", () => {
    expect(toolArguments({ path: "a.txt" })).toEqual({ path: "a.txt" });
    expect(
      toolArguments({
        callId: "call.1",
        providerCallId: null,
        capabilityId: "tool.job.list",
        name: "job_list",
      }),
    ).toEqual({});
  });

  it("formats each argument as one labelled value, never raw JSON", () => {
    const segments = toolArgumentSegments({ path: "reports/summary.md" });
    expect(segments).toEqual([
      { key: "path", label: "path", text: "reports/summary.md" },
    ]);
    expect(toolInputText(segments)).toBe("path: reports/summary.md");
    expect(toolInputText(toolArgumentSegments({ command: "git status", pattern: "*.ts" }))).toBe(
      "command: git status · pattern: *.ts",
    );
    expect(toolInputText(toolArgumentSegments({ query: "workflow", limit: 5 }))).toBe(
      "query: workflow · limit: 5",
    );
  });

  it("summarizes shapes it cannot show as text", () => {
    expect(toolInputText(toolArgumentSegments({ urls: ["https://a.test", "https://b.test"] }))).toBe(
      "urls: https://a.test, https://b.test",
    );
    expect(toolInputText(toolArgumentSegments({ options: [{ id: "a" }] }))).toBe(
      "options: 1 item",
    );
    expect(toolInputText(toolArgumentSegments({ goal: { status: "active" } }))).toBe(
      "goal: 1 field",
    );
    expect(toolArgumentSegments({ cursor: null, maximumBytes: 0 })).toEqual([
      { key: "maximumBytes", label: "maximum bytes", text: "0" },
    ]);
  });

  it("treats an argument that carries its own line breaks as multiline", () => {
    expect(toolInputIsMultiline(toolArgumentSegments({ command: "one\ntwo" }))).toBe(true);
    expect(toolInputIsMultiline(toolArgumentSegments({ command: "one two" }))).toBe(false);
    expect(toolInputIsMultiline([])).toBe(false);
  });

  it("finds the path only for the file tools that act on one", () => {
    expect(
      fileToolPath(
        item({
          metadata: { capabilityId: "tool.files.read" },
          input: { path: "reports/summary.md" },
        }),
      ),
    ).toBe("reports/summary.md");
    expect(
      fileToolPath(
        item({
          title: "tool.files.edit",
          metadata: { capabilityId: "tool.files.edit" },
          input: { arguments: { path: "a.ts", old_string: "a", new_string: "b" } },
        }),
      ),
    ).toBe("a.ts");
    expect(
      fileToolPath(
        item({
          title: "tool.shell.host",
          metadata: { capabilityId: "tool.shell.host" },
          input: { command: "ls" },
        }),
      ),
    ).toBeUndefined();
    expect(
      fileToolPath(
        item({
          metadata: { capabilityId: "tool.files.read" },
          input: { path: `/${"a".repeat(4_096)}` },
        }),
      ),
    ).toBeUndefined();
    expect(
      fileToolPath(
        item({
          kind: "mcp",
          metadata: { capabilityId: "tool.files.read" },
          input: { path: "a.txt" },
        }),
      ),
    ).toBeUndefined();
  });
});

describe("the tool's own output", () => {
  it("extracts text from the canonical result envelope and its payload", () => {
    expect(
      toolResultText({ callId: "call.1", isError: false, content: { stdout: "42\n", exitCode: 0 } }),
    ).toBe("42\n");
    expect(
      toolResultText({ callId: "call.1", content: { files: ["notes.txt", "readme.md"] } }),
    ).toBe("notes.txt\nreadme.md");
    expect(toolResultText("plain result")).toBe("plain result");
    expect(toolResultText([{ kind: "assistant_output", text: "part one" }, { kind: "assistant_output", text: "part two" }])).toBe(
      "part one\npart two",
    );
  });

  it("reports no text for a result that carries none", () => {
    expect(toolResultText({ callId: "call.1", content: { exits: 3, clean: true } })).toBeUndefined();
    expect(toolResultText({})).toBeUndefined();
    expect(toolResultText(null)).toBeUndefined();
    expect(toolResultText("   ")).toBeUndefined();
  });

  it("reduces a text-free result to a compact status", () => {
    expect(toolResultStatus(undefined)).toBe("No output");
    expect(toolResultStatus({ callId: "call.1", isError: true, content: { retries: 3 } })).toBe(
      "Failed with no text output",
    );
    expect(toolResultStatus({ callId: "call.1", content: { stdout: "", stderr: "", exitCode: 3 } })).toBe(
      "No output (exit code 3)",
    );
    expect(toolResultStatus({ callId: "call.1", content: { stdout: "", exitCode: 0 } })).toBe(
      "No output (exit code 0)",
    );
    expect(toolResultStatus([1, 2, 3])).toBe("List of 3 items");
    expect(toolResultStatus([])).toBe("Empty list");
    expect(toolResultStatus({ callId: "call.1", content: { a: 1, b: 2 } })).toBe(
      "Structured result (2 fields)",
    );
    expect(toolResultStatus({ callId: "call.1", content: {} })).toBe("Empty result");
  });

  it("streams a running tool's progress and shows the settled output", () => {
    const running = item({
      status: "running",
      metadata: { capabilityId: "tool.shell.host", live: true, channels: { progress: "line one\n" } },
    });
    expect(toolOutputView(running)).toEqual({
      kind: "text",
      text: "line one\n",
      streaming: true,
    });
    expect(toolOutputView(item({ output: { content: { stdout: "done\n" } } }))).toEqual({
      kind: "text",
      text: "done\n",
      streaming: false,
    });
    expect(toolOutputView(item({ output: { content: { ok: true } } }))).toEqual({
      kind: "status",
      text: "Structured result (1 field)",
    });
    expect(
      toolOutputView(item({ status: "running", metadata: { live: true } })),
    ).toBeUndefined();
    expect(toolOutputView(item({ body: "notes.txt" }))).toEqual({
      kind: "text",
      text: "notes.txt",
      streaming: false,
    });
  });

  it("prefers the projected image attachments over any text extraction", () => {
    const images = [
      {
        id: "a".repeat(64),
        name: "shot.png",
        mimeType: "image/png" as const,
        byteLength: 3,
      },
    ];
    expect(toolOutputView(item({ attachments: images, output: "ignored" }))).toEqual({
      kind: "image",
      images,
    });
  });
});
