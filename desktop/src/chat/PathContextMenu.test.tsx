// @vitest-environment jsdom
import { isTauri } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { PathActionOutcome, RunPathAction } from "../adapters/contracts";
import { NotificationProvider } from "../notifications/NotificationContext";
import { NotificationStore } from "../notifications/NotificationStore";
import { TimelineCard } from "./ConversationTimeline";
import { toConversationCard } from "./conversation";
import { MarkdownContent } from "./MarkdownContent";
import { PathActionProvider } from "./pathActions";
import type { TimelineItem } from "./types";

vi.mock("@tauri-apps/api/core", () => ({ isTauri: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));

beforeEach(() => {
  vi.mocked(isTauri).mockReturnValue(true);
});
afterEach(cleanup);

const RESOLVED: PathActionOutcome = {
  absolutePath: "/work/reports/summary.md",
  eligible: true,
  performed: false,
};

function renderPath(link: string, run: RunPathAction) {
  const store = new NotificationStore();
  render(
    <NotificationProvider store={store}>
      <PathActionProvider run={run}>
        <MarkdownContent>{link}</MarkdownContent>
      </PathActionProvider>
    </NotificationProvider>,
  );
  return store;
}

function openMenu(label = "report") {
  const link = screen.getByText(label);
  fireEvent.contextMenu(link, { clientX: 40, clientY: 60 });
  return link;
}

describe("path context menu", () => {
  it("resolves a relative path and offers the three OS actions", async () => {
    const run = vi.fn<RunPathAction>(async (_path, action) =>
      action === "inspect" ? RESOLVED : { ...RESOLVED, performed: true },
    );
    renderPath("[report](reports/summary.md)", run);

    openMenu();

    expect(await screen.findByRole("menu")).toBeVisible();
    expect(run).toHaveBeenCalledExactlyOnceWith("reports/summary.md", "inspect");
    // The resolved location is shown before anything is opened.
    expect(screen.getByText("/work/reports/summary.md")).toBeVisible();
    for (const name of [
      "Open in default application",
      "Open in editor",
      "Reveal in folder",
    ])
      expect(screen.getByRole("menuitem", { name })).toBeEnabled();
    // Resolving on its own never performs an action.
    expect(run).toHaveBeenCalledTimes(1);
  });

  it("keeps a refused path inert and explains why", async () => {
    const run = vi.fn<RunPathAction>(async () => ({
      absolutePath: "/elsewhere/private.md",
      eligible: false,
      reason: "the path is outside the Chat's workspace",
      performed: false,
    }));
    renderPath("[private](/elsewhere/private.md)", run);

    openMenu("private");

    expect(
      await screen.findByText("the path is outside the Chat's workspace"),
    ).toBeVisible();
    for (const name of [
      "Open in default application",
      "Open in editor",
      "Reveal in folder",
    ])
      expect(screen.getByRole("menuitem", { name })).toBeDisabled();
  });

  it("performs the chosen action once and reports a refusal", async () => {
    const run = vi.fn<RunPathAction>(async (_path, action) =>
      action === "inspect"
        ? RESOLVED
        : {
            ...RESOLVED,
            eligible: true,
            performed: false,
            reason: "the file no longer exists",
          },
    );
    const store = renderPath("[report](reports/summary.md)", run);
    openMenu();
    await screen.findByRole("menu");

    fireEvent.click(screen.getByRole("menuitem", { name: "Open in editor" }));

    expect(run).toHaveBeenLastCalledWith("reports/summary.md", "open_editor");
    await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
    expect(store.getSnapshot().active).toEqual([
      expect.objectContaining({
        severity: "error",
        summary: "Could not open the file in your editor.",
      }),
    ]);
    store.dispose();
  });

  it("reports a core failure instead of opening anything", async () => {
    const run = vi.fn<RunPathAction>(async (_path, action) => {
      if (action === "inspect") return RESOLVED;
      throw new Error("the desktop runtime lock is unavailable");
    });
    const store = renderPath("[report](reports/summary.md)", run);
    openMenu();
    await screen.findByRole("menu");

    fireEvent.click(screen.getByRole("menuitem", { name: "Reveal in folder" }));

    await waitFor(() =>
      expect(store.getSnapshot().active).toEqual([
        expect.objectContaining({
          severity: "error",
          summary: "Could not reveal the file.",
          detail: expect.stringContaining("the desktop runtime lock is unavailable"),
        }),
      ]),
    );
    expect(screen.queryByRole("menu")).toBeNull();
    store.dispose();
  });

  it("leaves web citations and unpreviewed paths without a menu", () => {
    const run = vi.fn<RunPathAction>(async () => RESOLVED);
    renderPath("[Forecast](https://example.test/weather)", run);

    const link = screen.getByText("Forecast");
    expect(fireEvent.contextMenu(link)).toBe(true);
    expect(screen.queryByRole("menu")).toBeNull();
    expect(run).not.toHaveBeenCalled();
  });

  it("closes on Escape and on a click outside the menu", async () => {
    const run = vi.fn<RunPathAction>(async () => RESOLVED);
    renderPath("[report](reports/summary.md)", run);
    openMenu();
    await screen.findByRole("menu");

    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());

    openMenu();
    await screen.findByRole("menu");
    window.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true }));
    await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());

    // A click inside the menu keeps it open.
    openMenu();
    const item = await screen.findByRole("menuitem", { name: "Reveal in folder" });
    item.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true }));
    expect(screen.getByRole("menu")).toBeVisible();
  });
});

function toolItem(
  capabilityId: string,
  input: Record<string, unknown>,
): TimelineItem {
  return {
    id: "span.tool.1",
    kind: "tool",
    title: capabilityId,
    createdAt: "2026-01-01T00:00:00Z",
    status: "completed",
    metadata: { capabilityId },
    input,
  };
}

function renderToolCard(item: TimelineItem, run: RunPathAction) {
  const store = new NotificationStore();
  render(
    <NotificationProvider store={store}>
      <PathActionProvider run={run}>
        <TimelineCard
          card={toConversationCard(item)}
          item={item}
          selected={false}
          onAction={vi.fn()}
          onSelect={vi.fn()}
        />
      </PathActionProvider>
    </NotificationProvider>,
  );
  return store;
}

describe("file tool card paths", () => {
  it("names the file the tool touched and offers the actions on it", async () => {
    const run = vi.fn<RunPathAction>(async (_path, action) =>
      action === "inspect" ? RESOLVED : { ...RESOLVED, performed: true },
    );
    renderToolCard(toolItem("tool.files.read", { path: "reports/summary.md" }), run);

    const path = screen.getByText("reports/summary.md");
    expect(path).toBeVisible();

    fireEvent.contextMenu(path, { clientX: 12, clientY: 18 });

    expect(await screen.findByRole("menu")).toBeVisible();
    expect(run).toHaveBeenCalledExactlyOnceWith(
      "reports/summary.md",
      "inspect",
    );
  });

  it("leaves a tool that takes no path without any path action", () => {
    const run = vi.fn<RunPathAction>(async () => RESOLVED);
    const item = toolItem("tool.shell.run", { command: "ls" });
    renderToolCard(item, run);

    // The card keeps showing the tool's own content, and a right-click on it
    // must not offer to open a command as if it were a file.
    const content = screen.getAllByText("tool.shell.run")[1]!;
    expect(fireEvent.contextMenu(content)).toBe(true);
    expect(screen.queryByRole("menu")).toBeNull();
    expect(run).not.toHaveBeenCalled();
  });

  it("keeps a file tool card inert without a path action port", () => {
    const item = toolItem("tool.files.read", { path: "reports/summary.md" });
    render(
      <TimelineCard
        card={toConversationCard(item)}
        item={item}
        selected={false}
        onAction={vi.fn()}
        onSelect={vi.fn()}
      />,
    );

    // The path stays visible, but neither a left nor a right click acts on it.
    const path = screen.getByText("reports/summary.md");
    expect(fireEvent.click(path)).toBe(true);
    expect(fireEvent.contextMenu(path)).toBe(true);
    expect(screen.queryByRole("menu")).toBeNull();
  });
});
