// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { RawJsonView } from "./RawJsonView";
import { RunDetailsInspector } from "./RunDetailsInspector";
import { rawJsonPages, RAW_JSON_PAGE_CHARACTERS } from "./rawJsonPages";
import type { ChatProjection } from "./types";

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("bounded Raw JSON rendering", () => {
  it("preserves every character across parts, including huge strings and Unicode boundaries", () => {
    const samples = ["", '{\n  "small": true\n}', "row\n".repeat(30_000),
      "x".repeat(RAW_JSON_PAGE_CHARACTERS - 1) + "😀" + "y".repeat(100_000)];
    for (const raw of samples) {
      const pages = rawJsonPages(raw);
      expect(pages.join("")).toBe(raw);
      expect(pages.every(page => page.length <= RAW_JSON_PAGE_CHARACTERS)).toBe(true);
      expect(pages.some(page => /[\uD800-\uDBFF]$/.test(page))).toBe(false);
    }
  });

  it("bounds the DOM, navigates to the final data, and copies the complete JSON", () => {
    const value = { long: "a ".repeat(100_000), final: "END_OF_COMPLETE_JSON" };
    const raw = JSON.stringify(value, null, 2);
    const copy = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText: copy } });
    const { container, rerender } = render(<RawJsonView value={value} />);
    const text = () => container.querySelector("pre")!.textContent!;
    expect(text().length).toBeLessThanOrEqual(RAW_JSON_PAGE_CHARACTERS);
    expect(screen.getByRole("button", { name: "Previous JSON part" })).toBeDisabled();
    const input = screen.getByRole("spinbutton", { name: "JSON part" });
    fireEvent.change(input, { target: { value: input.getAttribute("max") } });
    expect(text()).toContain("END_OF_COMPLETE_JSON");
    expect(screen.getByRole("button", { name: "Next JSON part" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Copy JSON" }));
    expect(copy).toHaveBeenCalledWith(raw);
    rerender(<RawJsonView value={{ updated: true }} />);
    expect(text()).toBe(JSON.stringify({ updated: true }, null, 2));
    expect(screen.queryByRole("navigation", { name: "Raw JSON parts" })).toBeNull();
  });

  it("serializes only when Raw JSON is selected and reuses the value on layout rerenders", () => {
    const serialize = vi.fn(() => ({ diagnostic: "fixture" }));
    const chat: ChatProjection = { chatId: "chat.test", runId: "run.test", title: "Complete run", scope: "Local",
      workflowId: null, workflowName: null, branch: null, projectId: null, phase: "completed",
      lockedWorkflow: true, recoveryPending: false, queuedInputs: [], expectedVersion: 1 };
    const props = { chat, events: [], items: [], records: [{ id: "diagnostic", category: "debug" as const, label: "Diagnostic",
      state: "available" as const, value: { toJSON: serialize } }], selectedId: null, onSelect: vi.fn(), onClose: vi.fn() };
    const { rerender } = render(<RunDetailsInspector {...props} />);
    expect(serialize).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("tab", { name: "Raw JSON" }));
    expect(serialize).toHaveBeenCalledOnce();
    rerender(<RunDetailsInspector {...props} onClose={vi.fn()} />);
    expect(serialize).toHaveBeenCalledOnce();
  });
});
