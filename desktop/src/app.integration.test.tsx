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
    expect(document.querySelector(".composer-footer .run-status")).toHaveTextContent(
      "Waiting for input",
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
  }, 30_000);
  // Entering and leaving Settings twice through the whole App shell costs
  // several seconds; the budget is headroom, not an expectation.


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


  it("provides accessible in-workbench notification and confirmation fallbacks", async () => {
    const user = userEvent.setup();
    render(<App adapters={defaultDesktopAdapters} />);
    await act(async () =>
      defaultDesktopAdapters.nativePresentation.notify(
        "Run complete",
        "All committed events are visible.",
      ),
    );
    expect(document.querySelector(".notification-message")).toHaveTextContent("Run complete");
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
