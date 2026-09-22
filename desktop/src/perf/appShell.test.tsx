// @vitest-environment jsdom
/*
 * Wall-clock gates for the whole App shell.
 *
 * These render the complete application and type with real events, so they
 * cost seconds each and fail spuriously under parallel load. They are an
 * opt-in gate (`pnpm test:perf`), never part of the default suite.
 */
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import { App } from "../App";
import { defaultDesktopAdapters } from "../adapters/defaultAdapters";
import { projectAppearancePreference } from "../workbench/appearance";

const lazyRouteWait = { timeout: 5_000 } as const;

afterEach(() => {
  cleanup();
  localStorage.clear();
  projectAppearancePreference("system");
});

describe("desktop shell wall-clock gates", () => {
  it("supports focus-safe navigation, workflow editing, settings, and splitters", async () => {
    const user = userEvent.setup();
    render(<App adapters={defaultDesktopAdapters} />);
    const splitter = screen.getByRole("separator", {
      name: "Resize navigation pane",
    });
    expect(splitter).toHaveAttribute("aria-valuenow", "208");
    fireEvent.keyDown(splitter, { key: "ArrowRight" });
    expect(splitter).toHaveAttribute("aria-valuenow", "216");

    await user.click(screen.getByRole("button", { name: /Workflows/ }));
    expect(
      await screen.findByRole("heading", { name: "Standard Agent" }, lazyRouteWait),
    ).toBeVisible();
    expect(screen.getByLabelText("Workflow graph")).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Import JSON" }),
    ).toBeEnabled();
    expect(
      screen.getByRole("button", {
        name: "Add Model Call node",
      }),
    ).toBeEnabled();
    expect(
      screen.getByRole("button", {
        name: "Add transition",
      }),
    ).toBeEnabled();
    expect(screen.getByRole("button", { name: /Validate/ })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Export" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Run" })).toBeEnabled();
    const inputNode = screen.getByRole("button", { name: "Input" });
    await user.click(inputNode);
    expect(
      screen.getByRole("button", { name: "Delete node" }),
    ).toBeEnabled();
    expect(screen.getByLabelText("Node type")).toBeEnabled();
    fireEvent.keyDown(inputNode, { altKey: true, key: "ArrowRight" });
    expect(screen.getByRole("button", { name: "Save" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Run" })).toBeDisabled();

    await user.click(screen.getByRole("button", { name: /Settings/ }));
    expect(
      await screen.findByRole("heading", { name: "Settings" }, lazyRouteWait),
    ).toBeVisible();
    await user.click(screen.getByRole("button", { name: /Appearance/ }));
    await user.click(screen.getByRole("radio", { name: /Dark/ }));
    await user.click(
      screen.getByRole("button", { name: "Save configuration" }),
    );
    expect(document.documentElement.dataset.appearance).toBe("dark");

    await user.click(screen.getByRole("button", { name: "Back to Workflows" }));
    await screen.findByRole("heading", { name: "Standard Agent" }, lazyRouteWait);
    expect(screen.getByRole("button", { name: "Save" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Input" })).toBe(inputNode);
    await user.click(screen.getByRole("button", { name: /Undo/ }));
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
  }, 10_000);

  it("Escape closes notification details first, then guards dirty Settings until Discard", async () => {
    const user = userEvent.setup();
    render(<App adapters={defaultDesktopAdapters} />);
    await screen.findByRole("textbox", { name: "Chat input" });
    fireEvent.keyDown(window, { key: ",", ctrlKey: true });
    await screen.findByRole("button", { name: "Back to Chat" }, lazyRouteWait);
    await user.click(screen.getByRole("button", { name: /Appearance/ }));
    await user.click(screen.getByRole("radio", { name: /Dark/ }));
    const list = screen.getByRole("button", { name: /Notifications, / });
    await user.click(list);
    fireEvent.keyDown(list, { key: "Escape" });
    expect(screen.queryByRole("region", { name: "Notification details" })).toBeNull();
    expect(screen.queryByRole("dialog")).toBeNull();
    fireEvent.keyDown(window, { key: "Escape" });
    const stay = screen.getByRole("button", { name: "Stay in Settings" });
    fireEvent.keyDown(stay, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.getByRole("radio", { name: /Dark/ })).toBeChecked();
    await user.click(screen.getByRole("button", { name: "Back to Chat" }));
    await user.click(screen.getByRole("button", { name: "Discard and return" }));
    expect(await screen.findByRole("textbox", { name: "Chat input" })).toBeVisible();
    expect(document.documentElement.dataset.appearance).toBe("light");
    // This flow drives the whole App shell through jsdom and finishes just past
    // Vitest's 5s default from interaction cost alone, not from a failing wait.
  }, 30_000);

  it("guards Settings navigation and preserves a complete unsaved provider draft", async () => {
    const user = userEvent.setup();
    render(<App adapters={defaultDesktopAdapters} />);
    await user.click(screen.getByRole("button", { name: /Settings/ }));
    await screen.findByRole("heading", { name: "Settings" }, lazyRouteWait);
    await user.click(
      await screen.findByRole("button", { name: "Add" }, lazyRouteWait),
    );
    const baseUrl = screen.getByLabelText("Base URL");
    await user.clear(baseUrl);
    await user.type(baseUrl, "http://localhost:11434/v1");
    await user.click(screen.getByRole("button", { name: "Add model" }));
    const remoteModel = screen.getByLabelText("Remote model ID");
    await user.type(remoteModel, "qwen3");
    expect(screen.queryByText(/validation issue$/u)).toBeNull();
    expect(
      screen.getByRole("button", { name: "Save configuration" }),
    ).toBeEnabled();
    await user.click(screen.getByRole("button", { name: "New Chat" }));
    await user.click(await screen.findByRole("button", { name: "Stay in Settings" }));
    expect(screen.getByLabelText("Base URL")).toHaveValue("http://localhost:11434/v1");
    expect(screen.getByLabelText("Remote model ID")).toHaveValue("qwen3");
    await user.click(screen.getByRole("button", { name: "Back to Chat" }));
    await user.click(await screen.findByRole("button", { name: "Save and return" }));
    await screen.findByRole("heading", { name: "New Chat" });
    await user.click(screen.getByRole("button", { name: /Settings/ }));
    expect(await screen.findByLabelText("Base URL")).toHaveValue(
      "http://localhost:11434/v1",
    );
    expect(screen.getByLabelText("Remote model ID")).toHaveValue("qwen3");
    // Typing into the full App shell and re-mounting the lazy Settings route costs
    // far more than Vitest's 5s default; every assertion above still holds.
  }, 90_000);
});
