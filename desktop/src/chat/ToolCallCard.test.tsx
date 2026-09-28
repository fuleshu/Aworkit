// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { projectSemanticTimeline } from "./activityProjection";
import { TimelineCard } from "./ConversationTimeline";
import { toConversationCard } from "./conversation";
import type { RuntimeEvent } from "./corePort";
import type { TimelineItem } from "./types";

vi.mock("./images", async (original) => ({
  ...(await original<typeof import("./images")>()),
  chatImagePreview: vi.fn(async () => "data:image/png;base64,aGVsbG8="),
}));

afterEach(cleanup);

const toolCallCss = readFileSync(
  resolve(process.cwd(), "src/chat/toolCallCard.css"),
  "utf8",
);

function renderCard(item: TimelineItem, onSelect: (id: string) => void = () => undefined) {
  return render(
    <TimelineCard
      card={toConversationCard(item)}
      item={item}
      selected={false}
      onSelect={onSelect}
      onAction={() => undefined}
    />,
  );
}

function toolItem(overrides: Partial<TimelineItem> = {}): TimelineItem {
  return {
    id: "span.tool.1",
    kind: "tool",
    title: "tool.files.read",
    createdAt: "2026-01-01T00:00:00Z",
    status: "completed",
    ...overrides,
  };
}

function field(card: HTMLElement, role: "tool-input" | "tool-output"): HTMLElement {
  const element = card.querySelector<HTMLElement>(`[data-role="${role}"]`);
  if (element === null) throw new Error(`No ${role} field on the card`);
  return element;
}

describe("tool-call card header", () => {
  it("AC1: shows the capability id followed by the real tool in brackets", () => {
    renderCard(
      toolItem({
        title: "tool.shell.host",
        metadata: { capabilityId: "tool.shell.host" },
        input: { command: "uname -s" },
      }),
    );

    const card = screen.getByRole("article", { name: "Tool: tool.shell.host" });
    // jsdom reports a non-Windows runtime identity, so the host shell is bash.
    expect(within(card).getByText("tool.shell.host (bash)")).toBeVisible();
  });

  it("AC1: degrades an unmapped capability id to the id with no empty brackets", () => {
    renderCard(toolItem({ title: "mcp://server/tool", metadata: {} }));

    const card = screen.getByRole("article", { name: "Tool: mcp://server/tool" });
    expect(within(card).getByText("mcp://server/tool")).toBeVisible();
    expect(within(card).queryByText(/\(\)/u)).toBeNull();
  });

  it("AC7: keeps the tool icon, the status chip and Inspect source record", () => {
    const item = toolItem({ status: "completed", raw: [{ kind: "span.completed" }] });
    renderCard(item);

    const card = screen.getByRole("article", { name: "Tool: tool.files.read" });
    expect(card).not.toHaveAttribute("aria-busy");
    expect(within(card).getByText(">_")).toBeVisible();
    expect(within(card).getByText("completed")).toBeVisible();
    expect(within(card).getByText("Inspect source record")).toBeVisible();
  });

  it("AC7: keeps the card selectable for Run details", async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    renderCard(toolItem(), onSelect);

    await user.click(screen.getByTitle("Show Run details for tool.files.read"));
    expect(onSelect).toHaveBeenCalledExactlyOnceWith("span.tool.1");
  });
});

describe("tool-call card input", () => {
  it("AC2: renders one formatted argument line instead of raw JSON", () => {
    renderCard(
      toolItem({
        input: {
          callId: "call.1",
          providerCallId: "call.1",
          capabilityId: "tool.files.read",
          name: "read_file",
          arguments: { path: "reports/summary.md", offset: 20, limit: 40 },
        },
        metadata: { capabilityId: "tool.files.read" },
      }),
    );

    const card = screen.getByRole("article", { name: "Tool: tool.files.read" });
    const input = field(card, "tool-input");
    expect(input.textContent).toBe("path: reports/summary.md · offset: 20 · limit: 40");
    expect(input).not.toHaveAttribute("data-multiline");
    expect(input).not.toHaveAttribute("data-expanded");
    expect(within(card).queryByText(/\{"/u)).toBeNull();
  });

  it("AC3: offers a capped expand for a multiline argument", async () => {
    const user = userEvent.setup();
    renderCard(
      toolItem({
        title: "tool.shell.host",
        metadata: { capabilityId: "tool.shell.host" },
        input: { command: "set -e\nmake check" },
      }),
    );

    const card = screen.getByRole("article", { name: "Tool: tool.shell.host" });
    const input = field(card, "tool-input");
    expect(input.textContent).toBe("command: set -e\nmake check");
    expect(input).toHaveAttribute("data-multiline", "true");
    expect(input).toHaveAttribute("data-rows", "1");
    expect(input.style.getPropertyValue("--aw-tool-rows")).toBe("1");

    await user.click(within(card).getByRole("button", { name: "Expand tool input" }));

    expect(input).toHaveAttribute("data-expanded", "true");
    expect(input).toHaveAttribute("data-rows", "10");
    expect(input.style.getPropertyValue("--aw-tool-rows")).toBe("10");
    expect(
      within(card).getByRole("button", { name: "Collapse tool input" }),
    ).toBeVisible();
  });

  it("AC3: caps the field in rows and scrolls only when it is expanded", () => {
    // The cap is expressed in code rows, so it follows text scaling; the
    // collapsed field clips, and the expanded one is the scrolling one.
    expect(toolCallCss).toContain("--aw-tool-rows");
    expect(toolCallCss).toMatch(
      /max-height:\s*calc\(var\(--aw-tool-rows[^)]*\)/u,
    );
    expect(toolCallCss).toMatch(
      /\[data-expanded="true"\]\s*\{[^}]*overflow:\s*auto/u,
    );
  });

  it("offers no expand control for a single-line argument", () => {
    renderCard(toolItem({ input: { path: "a.txt" } }));

    const card = screen.getByRole("article", { name: "Tool: tool.files.read" });
    expect(
      within(card).queryByRole("button", { name: "Expand tool input" }),
    ).toBeNull();
  });
});

describe("tool-call card output", () => {
  it("AC4: grows a running tool's output as progress deltas arrive", () => {
    const started = event(1, "span.started", {
      spanId: "span.tool.stream",
      spanKind: "tool_call",
      semanticRole: "tool",
      title: "tool.shell.host",
      capabilityId: "tool.shell.host",
      status: "running",
      hasInput: true,
      input: { command: "make check" },
    });
    const firstDelta = event(2, "span.content_delta", {
      spanId: "span.tool.stream",
      channel: "progress",
      append: "compiling\n",
    });
    const secondDelta = event(3, "span.content_delta", {
      spanId: "span.tool.stream",
      channel: "progress",
      append: "linking\n",
    });

    const running = projectSemanticTimeline([started, firstDelta])[0]!;
    const { rerender } = renderCard(running);
    const card = screen.getByRole("article", { name: "Tool: tool.shell.host" });
    const output = field(card, "tool-output");
    expect(output.textContent).toBe("compiling\n");
    expect(card).toHaveAttribute("aria-busy", "true");

    const grown = projectSemanticTimeline([started, firstDelta, secondDelta])[0]!;
    rerender(
      <TimelineCard
        card={toConversationCard(grown)}
        item={grown}
        selected={false}
        onSelect={() => undefined}
        onAction={() => undefined}
      />,
    );

    // Same card, same field: the text grew without a manual refresh or reopen.
    expect(output.textContent).toBe("compiling\nlinking\n");
    expect(screen.getByRole("article", { name: "Tool: tool.shell.host" })).toBe(card);
  });

  it("AC5: defaults to four rows, expands to ten and follows the newest line", async () => {
    const user = userEvent.setup();
    const item = toolItem({
      output: { callId: "call.1", content: { stdout: "one\ntwo\nthree\nfour" } },
    });
    const { rerender } = renderCard(item);

    const card = screen.getByRole("article", { name: "Tool: tool.files.read" });
    const output = field(card, "tool-output");
    expect(output.textContent).toBe("one\ntwo\nthree\nfour");
    expect(output).toHaveAttribute("data-rows", "4");
    expect(output.style.getPropertyValue("--aw-tool-rows")).toBe("4");

    const scrollTop = vi.fn();
    Object.defineProperty(output, "scrollHeight", {
      configurable: true,
      value: 350,
    });
    Object.defineProperty(output, "scrollTop", {
      configurable: true,
      get: () => 0,
      set: scrollTop,
    });

    const streamed = toolItem({
      status: "running",
      metadata: {
        capabilityId: "tool.files.read",
        live: true,
        channels: { progress: "one\ntwo\nthree\nfour\nfive" },
      },
    });
    rerender(
      <TimelineCard
        card={toConversationCard(streamed)}
        item={streamed}
        selected={false}
        onSelect={() => undefined}
        onAction={() => undefined}
      />,
    );
    // The newest line stays visible while the tool streams.
    expect(scrollTop).toHaveBeenLastCalledWith(350);

    await user.click(
      screen.getByRole("button", { name: "Expand tool output" }),
    );
    expect(output).toHaveAttribute("data-rows", "10");
    expect(output.style.getPropertyValue("--aw-tool-rows")).toBe("10");
    expect(output).toHaveAttribute("data-expanded", "true");
  });

  it("AC6: renders an image result instead of JSON", async () => {
    const image = {
      id: "a".repeat(64),
      name: "shot.png",
      mimeType: "image/png",
      byteLength: 3,
    };
    const item = projectSemanticTimeline([
      event(1, "span.started", {
        spanId: "span.tool.image",
        spanKind: "tool_call",
        semanticRole: "tool",
        title: "tool.screenshot",
        capabilityId: "tool.screenshot",
        status: "running",
      }),
      event(2, "span.completed", {
        spanId: "span.tool.image",
        status: "completed",
        hasOutput: true,
        output: { callId: "capture.1", content: { image }, images: [image], isError: false },
      }),
    ])[0]!;

    renderCard(item);

    const card = screen.getByRole("article", { name: "Tool: tool.screenshot" });
    expect(
      await within(card).findByRole("img", { name: "shot.png" }),
    ).toBeVisible();
    expect(within(card).queryByText(/\{"/u)).toBeNull();
  });

  it("AC6: reduces a non-text, non-image result to a compact status", () => {
    renderCard(
      toolItem({
        output: { callId: "call.1", isError: false, content: { uploaded: 3, clean: true } },
      }),
    );

    const card = screen.getByRole("article", { name: "Tool: tool.files.read" });
    const output = field(card, "tool-output");
    expect(output.textContent).toBe("Structured result (2 fields)");
    expect(output).toHaveClass("tool-io-status");
    expect(within(card).queryByRole("button", { name: "Expand tool output" })).toBeNull();
  });
});

function event(
  sequence: number,
  kind: string,
  payload: Record<string, unknown>,
): RuntimeEvent {
  return {
    schemaVersion: 1,
    streamId: "chat.test",
    branchId: "main",
    sequence,
    eventId: `event.chat.${sequence}`,
    kind,
    payload,
  };
}
