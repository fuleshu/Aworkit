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

describe("honest JSON-workflow desktop slice", () => {
  it("renders a clean draft Chat without fabricated projects, history, or run activity", async () => {
    const { container } = render(<App adapters={defaultDesktopAdapters} />);
    expect(
      await screen.findByRole("heading", { name: "New Chat" }),
    ).toBeVisible();
    const navigation = screen.getByRole("navigation", {
      name: "Primary navigation",
    });
    expect(navigation).toHaveTextContent("New Chat");
    expect(navigation).toHaveTextContent("Chat");
    expect(navigation).toHaveTextContent("Settings");
    expect(navigation).not.toHaveTextContent("Project Atlas");
    expect(
      screen.getByRole("button", { name: /Management Chat.*Unsupported/ }),
    ).toBeDisabled();
    expect(document.querySelector(".chat-view-header .run-status")).toHaveTextContent(
      "Draft",
    );
    expect(
      screen.getByRole("combobox", {
        name: "Workflow for the first Chat input",
      }),
    ).toHaveValue("workflow.standard-agent");
    expect(
      screen.getByRole("button", { name: "Add attachment" }),
    ).toBeEnabled();
    expect(
      screen.getByRole("button", { name: "Add attachment" }),
    ).toHaveAttribute("title", "Add images");
    expect(screen.getByText(/No messages yet/)).toBeVisible();
    const runDetails = screen.getByRole("complementary", {
      name: "Run details",
    });
    expect(runDetails).toHaveTextContent("Entire run");
    expect(runDetails).toHaveTextContent("No execution activity has been recorded.");
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

  it("Back restores the exact Chat draft, caret, focus and inspector after repeated Settings entry", async () => {
    const user = userEvent.setup();
    render(<App adapters={defaultDesktopAdapters} />);
    const composer = await screen.findByRole("textbox", { name: "Chat input" }) as HTMLTextAreaElement;
    await user.type(composer, "an unsent draft with a selected word");
    fireEvent.keyDown(screen.getByRole("separator", { name: "Resize Run details" }), { key: "ArrowLeft" });
    composer.focus();
    composer.setSelectionRange(3, 9);
    fireEvent.keyDown(composer, { key: ",", ctrlKey: true });
    await screen.findByRole("button", { name: "Back to Chat" }, lazyRouteWait);
    await user.click(screen.getByTitle("Settings"));
    await user.click(screen.getByRole("button", { name: "Back to Chat" }));
    await waitFor(() => expect(composer).toHaveFocus());
    expect(composer).toHaveValue("an unsent draft with a selected word");
    expect([composer.selectionStart, composer.selectionEnd]).toEqual([3, 9]);
    expect(screen.getByRole("separator", { name: "Resize Run details" })).toHaveAttribute("aria-valuenow", "328");
    expect(screen.getByRole("textbox", { name: "Chat input" })).toBe(composer);
  });

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
  });

  it("opens the starter graph declared as default in the JSON bundle", async () => {
    const user = userEvent.setup();
    render(<App adapters={defaultDesktopAdapters} />);
    await user.click(screen.getByRole("button", { name: /Workflows/ }));
    await screen.findByRole("heading", { name: "Standard Agent" }, lazyRouteWait);
    expect(screen.getByRole("button", { name: "Input" })).toBeVisible();
    expect(screen.getByRole("button", { name: "Plan" })).toBeVisible();
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
      name: "Resize Run details",
    });
    expect(splitter).toHaveAttribute("aria-valuenow", "320");
    fireEvent.keyDown(splitter, { key: "ArrowLeft" });
    expect(splitter).toHaveAttribute("aria-valuenow", "328");
    await user.click(screen.getByRole("button", { name: /Workflows/ }));
    await screen.findByRole("heading", { name: "Standard Agent" }, lazyRouteWait);
    await user.click(screen.getByRole("button", { name: "New Chat" }));
    expect(
      await screen.findByRole("textbox", { name: "Chat input" }),
    ).toHaveValue("keep this local draft");
    expect(
      screen.getByRole("separator", { name: "Resize Run details" }),
    ).toHaveAttribute("aria-valuenow", "328");
  });

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
