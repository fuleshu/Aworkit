// @vitest-environment jsdom
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import axe from "axe-core";
import { afterEach, describe, expect, it } from "vitest";
import { App } from "./App";
import { defaultDesktopAdapters } from "./adapters/defaultAdapters";
import { projectAppearancePreference } from "./workbench/appearance";

const lazyRouteWait = { timeout: 5_000 } as const;

afterEach(() => {
  cleanup();
  localStorage.clear();
  projectAppearancePreference("system");
});

describe("honest Simple Chat desktop slice", () => {
  it("renders a clean draft Chat without fabricated projects, history, or evidence", async () => {
    const { container } = render(<App adapters={defaultDesktopAdapters} />);
    expect(
      await screen.findByRole("heading", { name: "New Chat" }),
    ).toBeVisible();
    const navigation = screen.getByRole("navigation", {
      name: "Primary navigation",
    });
    expect(navigation).toHaveTextContent("New Chat");
    expect(navigation).toHaveTextContent("Simple Chat");
    expect(navigation).toHaveTextContent("Settings");
    expect(navigation).not.toHaveTextContent("Project Atlas");
    expect(
      screen.getByRole("button", { name: /Management Chat.*Unsupported/ }),
    ).toBeDisabled();
    expect(screen.getByText("Draft")).toBeVisible();
    expect(
      screen.getByRole("combobox", {
        name: "Workflow for the first Chat input",
      }),
    ).toHaveValue("workflow.simple-chat");
    expect(
      screen.getByRole("button", { name: "Add attachment references" }),
    ).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "Add attachment references" }),
    ).toHaveAttribute("title", "Attachments are unsupported in this build");
    expect(screen.getByText(/No messages yet/)).toBeVisible();
    expect(
      screen.getByRole("complementary", { name: "Evidence inspector" }),
    ).toHaveTextContent("0 records");
    const results = await axe.run(container, {
      rules: { "color-contrast": { enabled: false } },
    });
    expect(results.violations).toEqual([]);
  });

  it("keeps the Chat draft when browser Preview honestly refuses provider execution", async () => {
    const user = userEvent.setup();
    render(<App adapters={defaultDesktopAdapters} />);
    const composer = await screen.findByRole("textbox", { name: "Chat input" });
    await user.type(composer, "Hello from Preview");
    await user.click(screen.getByRole("button", { name: "Send" }));
    expect(
      await screen.findByText(/requires the native desktop runtime/),
    ).toBeVisible();
    expect(composer).toHaveValue("Hello from Preview");
  });

  it("supports focus-safe navigation, Simple Chat editing, settings, and splitters", async () => {
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
      await screen.findByRole("heading", { name: "Simple Chat" }, lazyRouteWait),
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

    await user.click(screen.getByRole("button", { name: /Workflows/ }));
    await screen.findByRole("heading", { name: "Simple Chat" }, lazyRouteWait);
    expect(screen.getByRole("button", { name: "Save" })).toBeEnabled();
  }, 10_000);

  it("ships only the exact input-to-agent-to-output-to-wait starter graph", async () => {
    const user = userEvent.setup();
    render(<App adapters={defaultDesktopAdapters} />);
    await user.click(screen.getByRole("button", { name: /Workflows/ }));
    await screen.findByRole("heading", { name: "Simple Chat" }, lazyRouteWait);
    expect(screen.getByRole("button", { name: "Input" })).toBeVisible();
    expect(screen.getByRole("button", { name: "Agent" })).toBeVisible();
    expect(screen.getByRole("button", { name: "Output" })).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Wait for input" }),
    ).toBeVisible();
    expect(screen.queryByText("Missing dependency")).toBeNull();
    expect(screen.getByRole("button", { name: "Run" })).toBeEnabled();
    await user.click(screen.getByRole("button", { name: "Run" }));
    expect(
      await screen.findByRole("heading", { name: "New Chat" }, lazyRouteWait),
    ).toBeVisible();
  });

  it("preserves an unsent draft and inspector geometry across route handoff", async () => {
    const user = userEvent.setup();
    render(<App adapters={defaultDesktopAdapters} />);
    const composer = await screen.findByRole("textbox", { name: "Chat input" });
    await user.type(composer, "keep this local draft");
    const splitter = screen.getByRole("separator", {
      name: "Resize evidence inspector",
    });
    expect(splitter).toHaveAttribute("aria-valuenow", "320");
    fireEvent.keyDown(splitter, { key: "ArrowLeft" });
    expect(splitter).toHaveAttribute("aria-valuenow", "328");
    await user.click(screen.getByRole("button", { name: /Workflows/ }));
    await screen.findByRole("heading", { name: "Simple Chat" }, lazyRouteWait);
    await user.click(screen.getByRole("button", { name: /Simple Chat/ }));
    expect(
      await screen.findByRole("textbox", { name: "Chat input" }),
    ).toHaveValue("keep this local draft");
    expect(
      screen.getByRole("separator", { name: "Resize evidence inspector" }),
    ).toHaveAttribute("aria-valuenow", "328");
  });

  it("preserves a complete unsaved provider draft across route handoff", async () => {
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
    expect(
      screen.getByRole("button", { name: "Save configuration" }),
    ).toBeEnabled();
    await user.click(screen.getByRole("button", { name: /Simple Chat/ }));
    await screen.findByRole("heading", { name: "New Chat" });
    await user.click(screen.getByRole("button", { name: /Settings/ }));
    expect(screen.getByLabelText("Base URL")).toHaveValue(
      "http://localhost:11434/v1",
    );
    expect(screen.getByLabelText("Remote model ID")).toHaveValue("qwen3");
  });

  it("provides accessible in-workbench notification and confirmation fallbacks", async () => {
    const user = userEvent.setup();
    render(<App adapters={defaultDesktopAdapters} />);
    await act(async () =>
      defaultDesktopAdapters.nativePresentation.notify(
        "Run complete",
        "All committed events are visible.",
      ),
    );
    expect(screen.getByRole("status")).toHaveTextContent("Run complete");
    let confirmation: Promise<boolean> | undefined;
    await act(async () => {
      confirmation = defaultDesktopAdapters.nativePresentation.confirm(
        "Cancel Run?",
        "Completed effects remain committed.",
      );
    });
    expect(screen.getByRole("dialog")).toHaveAccessibleName("Cancel Run?");
    expect(screen.getByRole("button", { name: "Confirm" })).toHaveFocus();
    await user.tab();
    expect(screen.getByRole("button", { name: "Cancel" })).toHaveFocus();
    await user.tab({ shift: true });
    expect(screen.getByRole("button", { name: "Confirm" })).toHaveFocus();
    await user.click(screen.getByRole("button", { name: "Confirm" }));
    await expect(confirmation).resolves.toBe(true);
    let cancelled: Promise<boolean> | undefined;
    await act(async () => {
      cancelled = defaultDesktopAdapters.nativePresentation.confirm(
        "Discard draft?",
        "The local draft has not been committed.",
      );
    });
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    await expect(cancelled).resolves.toBe(false);
  });
});
