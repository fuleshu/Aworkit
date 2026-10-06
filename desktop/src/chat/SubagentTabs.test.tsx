// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  SubagentTabs,
  subagentStatusLabel,
  subagentTabLabel,
  subagentTabLabels,
} from "./SubagentTabs";
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
      "Subagent 1",
      "Subagent 2",
    ]);
    expect(tabs[0]).toHaveAttribute("aria-selected", "false");
    expect(tabs[1]).toHaveAttribute("aria-selected", "true");
    expect(tabs[0]).toHaveAttribute("aria-controls", "panel");
    // The active surface is the group, so one highlight paints behind the label
    // and its close button together.
    const activeGroup = tabs[1].closest(".subagent-tab-group");
    expect(activeGroup).toHaveClass("active");
    expect(activeGroup?.querySelector(".subagent-tab-close")).not.toBeNull();
    expect(tabs[2].closest(".subagent-tab-group")).not.toHaveClass("active");
    // The label truncates in its own element and can never run under the close
    // button.
    expect(tabs[1].querySelector(".subagent-tab-label")).not.toBeNull();
    // The parent tab is permanent: it has no close control.
    expect(screen.getAllByRole("button", { name: /^Close subagent tab/ })).toHaveLength(2);

    await user.click(tabs[0]);
    expect(onActivate).toHaveBeenCalledWith(null);
    await user.click(
      screen.getByRole("button", { name: "Close subagent tab Subagent 2" }),
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
    const childTab = screen.getByRole("tab", { name: "Subagent 1" });
    childTab.focus();
    fireEvent.keyDown(childTab, { key: "ArrowRight" });
    expect(onActivate).toHaveBeenLastCalledWith("child.b");
    fireEvent.keyDown(childTab, { key: "Home" });
    expect(onActivate).toHaveBeenLastCalledWith(null);
    fireEvent.keyDown(childTab, { key: "Delete" });
    expect(onClose).toHaveBeenCalledWith("child.a");
  });

  it("renders no strip while no delegated child tab is open", () => {
    // Settled children stay in the catalog, so their entries must not keep an
    // empty strip with a lone "Chat" tab on screen.
    render(
      <SubagentTabs
        entries={[entry("child.a", { status: "completed" })]}
        state={{ open: [], active: null }}
        panelId="panel"
        onActivate={vi.fn()}
        onClose={vi.fn()}
      />,
    );
    expect(screen.queryByRole("tablist")).not.toBeInTheDocument();
    expect(screen.queryByRole("tab", { name: "Chat" })).not.toBeInTheDocument();
  });

  it("renders no strip once every remembered tab left the catalog", () => {
    render(
      <SubagentTabs
        entries={[]}
        state={{ open: ["child.gone"], active: "child.gone" }}
        panelId="panel"
        onActivate={vi.fn()}
        onClose={vi.fn()}
      />,
    );
    expect(screen.queryByRole("tablist")).not.toBeInTheDocument();
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

  it("gives every child a short, stable, creation-ordered tab label", () => {
    const labels = subagentTabLabels([
      entry("child.b", { createdAt: "2026-01-02T00:00:00Z" }),
      entry("child.a", { createdAt: "2026-01-01T00:00:00Z" }),
      entry("child.fork", { kind: "fork", createdAt: "2026-01-03T00:00:00Z" }),
      entry("child.codex", {
        kind: "external",
        contextText: "External codex agent. Product permission mode: never.",
        createdAt: "2026-01-04T00:00:00Z",
      }),
      entry("child.claude", {
        kind: "external",
        contextText: "External claude-code agent. Product permission mode: dontAsk.",
        createdAt: "2026-01-05T00:00:00Z",
      }),
    ]);
    // Numbers follow creation order within each kind, so a settling sibling
    // never renumbers a running child.
    expect(labels.get("child.a")).toBe("Subagent 1");
    expect(labels.get("child.b")).toBe("Subagent 2");
    expect(labels.get("child.fork")).toBe("Fork 1");
    expect(labels.get("child.codex")).toBe("Codex 1");
    expect(labels.get("child.claude")).toBe("Claude Code 1");
    // An unrecognized external product still gets a usable label.
    expect(
      subagentTabLabels([
        entry("child.other", { kind: "external", contextText: "" }),
      ]).get("child.other"),
    ).toBe("External 1");
  });
});
