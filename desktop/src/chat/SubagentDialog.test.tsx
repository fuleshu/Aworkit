// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SubagentDialog } from "./SubagentDialog";
import type { SubagentCatalogEntry } from "./subagentCatalog";

afterEach(cleanup);

const entry = (
  childId: string,
  overrides: Partial<SubagentCatalogEntry> = {},
): SubagentCatalogEntry => ({
  childId,
  kind: "fresh",
  status: "completed",
  task: `Task for ${childId}`,
  contextText: "",
  finalText: "",
  depth: 1,
  headRevision: 1,
  modelTurns: 2,
  toolCalls: 1,
  inputTokens: 0,
  outputTokens: 0,
  ...overrides,
});

describe("composer subagents control", () => {
  it("is disabled while the Chat owns no child", () => {
    render(<SubagentDialog entries={[]} onOpenChild={vi.fn()} />);
    const button = screen.getByRole("button", { name: "No subagents in this Chat" });
    expect(button).toBeDisabled();
    expect(button).toHaveAttribute("aria-haspopup", "dialog");
  });

  it("lists this Chat's children running-first and opens the chosen tab", async () => {
    const user = userEvent.setup();
    const onOpenChild = vi.fn();
    render(
      <SubagentDialog
        entries={[
          entry("child.settled", { updatedAt: "2026-09-21T12:00:00Z" }),
          entry("child.live", { status: "running", updatedAt: "2026-09-21T09:00:00Z" }),
        ]}
        onOpenChild={onOpenChild}
      />,
    );
    const trigger = screen.getByRole("button", {
      name: "2 subagent(s) in this Chat, 1 running",
    });
    expect(trigger).toBeEnabled();
    await user.click(trigger);
    const dialog = screen.getByRole("dialog", { name: "Subagents in this Chat" });
    expect(dialog).toBeVisible();
    const options = screen.getAllByTitle(/Open the tab for subagent/);
    expect(options).toHaveLength(2);
    expect(options[0]).toHaveAttribute(
      "title",
      "Open the tab for subagent child.live",
    );
    expect(dialog).toHaveTextContent("1 running · 2 total");
    await user.click(options[0]);
    expect(onOpenChild).toHaveBeenCalledWith("child.live");
    expect(
      screen.queryByRole("dialog", { name: "Subagents in this Chat" }),
    ).toBeNull();
  });

  it("closes on Escape and restores focus to the trigger", async () => {
    const user = userEvent.setup();
    render(<SubagentDialog entries={[entry("child.a")]} onOpenChild={vi.fn()} />);
    const trigger = screen.getByRole("button", {
      name: "1 subagent(s) in this Chat, 0 running",
    });
    await user.click(trigger);
    fireEvent.keyDown(
      screen.getByRole("dialog", { name: "Subagents in this Chat" }),
      { key: "Escape" },
    );
    expect(
      screen.queryByRole("dialog", { name: "Subagents in this Chat" }),
    ).toBeNull();
    expect(trigger).toHaveFocus();
  });
});
