// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SubagentTabs, subagentStatusLabel, subagentTabLabel } from "./SubagentTabs";
import type { SubagentCatalogEntry } from "./subagentCatalog";

afterEach(cleanup);

const entry = (
  childId: string,
  overrides: Partial<SubagentCatalogEntry> = {},
): SubagentCatalogEntry => ({
  childId,
  kind: "fresh",
  status: "running",
  task: `Task for ${childId}`,
  contextText: "",
  finalText: "",
  depth: 1,
  headRevision: 1,
  modelTurns: 1,
  toolCalls: 0,
  inputTokens: 0,
  outputTokens: 0,
  ...overrides,
});

describe("subagent tab strip", () => {
  it("exposes a tablist with a permanent parent tab and one closable child tab", async () => {
    const user = userEvent.setup();
    const onActivate = vi.fn();
    const onClose = vi.fn();
    render(
      <SubagentTabs
        entries={[entry("child.a"), entry("child.b", { status: "completed" })]}
        state={{ open: ["child.a", "child.b"], active: "child.a" }}
        panelId="panel"
        onActivate={onActivate}
        onClose={onClose}
      />,
    );
    expect(
      screen.getByRole("tablist", { name: "Chat and subagent conversations" }),
    ).toBeVisible();
    const tabs = screen.getAllByRole("tab");
    expect(tabs.map((tab) => tab.textContent)).toEqual([
      "Chat",
      expect.stringContaining("Task for child.a"),
      expect.stringContaining("Task for child.b"),
    ]);
    expect(tabs[0]).toHaveAttribute("aria-selected", "false");
    expect(tabs[1]).toHaveAttribute("aria-selected", "true");
    expect(tabs[0]).toHaveAttribute("aria-controls", "panel");
    // The parent tab is permanent: it has no close control.
    expect(screen.getAllByRole("button", { name: /^Close subagent tab/ })).toHaveLength(2);

    await user.click(tabs[0]);
    expect(onActivate).toHaveBeenCalledWith(null);
    await user.click(
      screen.getByRole("button", { name: "Close subagent tab Task for child.b" }),
    );
    expect(onClose).toHaveBeenCalledWith("child.b");
  });

  it("moves the active tab with the arrow keys and closes with Delete", async () => {
    const onActivate = vi.fn();
    const onClose = vi.fn();
    render(
      <SubagentTabs
        entries={[entry("child.a"), entry("child.b")]}
        state={{ open: ["child.a", "child.b"], active: "child.a" }}
        panelId="panel"
        onActivate={onActivate}
        onClose={onClose}
      />,
    );
    const childTab = screen.getByRole("tab", { name: /Task for child.a/ });
    childTab.focus();
    fireEvent.keyDown(childTab, { key: "ArrowRight" });
    expect(onActivate).toHaveBeenLastCalledWith("child.b");
    fireEvent.keyDown(childTab, { key: "Home" });
    expect(onActivate).toHaveBeenLastCalledWith(null);
    fireEvent.keyDown(childTab, { key: "Delete" });
    expect(onClose).toHaveBeenCalledWith("child.a");
  });

  it("truncates long tasks and labels every status", () => {
    const long = entry("child.long", { task: "x".repeat(80) });
    expect(subagentTabLabel(long)).toHaveLength(48);
    expect(subagentTabLabel(long).endsWith("…")).toBe(true);
    expect(subagentTabLabel(entry("child.idle", { task: "  " }))).toBe("child.idle");
    expect(subagentStatusLabel("parent_approval_required")).toBe(
      "Needs parent approval",
    );
    expect(subagentStatusLabel("interrupted")).toBe("Interrupted");
  });
});
