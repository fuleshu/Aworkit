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

afterEach(() => {
  cleanup();
  localStorage.clear();
  projectAppearancePreference("system");
});

describe("Milestone 07–08 native desktop vertical slice", () => {
  it("renders the accepted desktop navigation and running Chat geometry", async () => {
    const { container } = render(<App adapters={defaultDesktopAdapters} />);
    expect(
      await screen.findByRole("heading", { name: "Release readiness" }),
    ).toBeVisible();
    const navigation = screen.getByRole("navigation", {
      name: "Primary navigation",
    });
    expect(navigation).toHaveTextContent("New Chat");
    expect(navigation).toHaveTextContent("Management Chat");
    expect(navigation).toHaveTextContent("Workflows");
    expect(navigation).toHaveTextContent("Project Atlas");
    expect(navigation).toHaveTextContent("Settings");
    expect(
      screen.getByRole("complementary", { name: "Evidence inspector" }),
    ).toBeVisible();
    expect(screen.getByRole("button", { name: /Pause/ })).toBeEnabled();
    expect(screen.getByText(/Workflow locked/)).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Add attachment references" }),
    ).toBeDisabled();
    expect(screen.getByText("1 queued input(s)")).toBeVisible();
    await userEvent.click(screen.getByText("1 queued input(s)"));
    expect(screen.getByText("Review the migration notes too.")).toBeVisible();
    const results = await axe.run(container, {
      rules: { "color-contrast": { enabled: false } },
    });
    expect(results.violations).toEqual([]);
  });

  it("dispatches real projection-shaped Run controls and retains drafts until confirmation", async () => {
    const user = userEvent.setup();
    render(<App adapters={defaultDesktopAdapters} />);
    await user.click(await screen.findByRole("button", { name: /Pause/ }));
    expect(await screen.findByRole("button", { name: /Resume/ })).toBeEnabled();
    const composer = screen.getByRole("textbox", { name: "Chat input" });
    await user.type(composer, "Review migration notes");
    await user.click(screen.getByRole("button", { name: "Queue" }));
    await waitFor(() => expect(composer).toHaveValue(""));
  });

  it("supports focus-safe navigation, workflow editing, settings drafts, and keyboard splitters", async () => {
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
      await screen.findByRole("heading", { name: "Repository Engineer" }),
    ).toBeVisible();
    expect(screen.getByLabelText("Workflow graph")).toBeVisible();
    await user.click(
      screen.getByRole("button", { name: "Add a Model node to the canvas" }),
    );
    expect(screen.getByRole("button", { name: /Save/ })).toBeEnabled();
    await user.click(screen.getByRole("button", { name: /Settings/ }));
    expect(
      await screen.findByRole("heading", { name: "Settings" }),
    ).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Appearance" }));
    expect(screen.getByRole("radio", { name: /Dark/ })).toBeVisible();
    await user.click(screen.getByRole("radio", { name: /Dark/ }));
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(document.documentElement.dataset.appearance).toBe("dark");
    await user.click(screen.getByRole("button", { name: /Workflows/ }));
    await screen.findByRole("heading", { name: "Repository Engineer" });
    expect(screen.getByRole("button", { name: "Save" })).toBeEnabled();
  });

  it("renders the accepted workflow dependency gate and enables Run only after resolution", async () => {
    const user = userEvent.setup();
    render(<App adapters={defaultDesktopAdapters} />);
    await user.click(screen.getByRole("button", { name: /Workflows/ }));
    expect(await screen.findByText("Missing dependency")).toBeVisible();
    expect(
      screen.getAllByText(/plugin\.code-review@2\.x/).length,
    ).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "Run" })).toBeDisabled();
    expect(
      screen.getByRole("button", {
        name: "Configure compatible capability…",
      }),
    ).toBeEnabled();
    fireEvent.keyDown(
      screen.getByRole("button", { name: "acme.code-review@2.x" }),
      { key: "ArrowRight", altKey: true },
    );
    await user.selectOptions(
      screen.getByRole("combobox", { name: "Transition source node" }),
      "input.1",
    );
    await user.selectOptions(
      screen.getByRole("combobox", { name: "Transition target node" }),
      "input.1",
    );
    await user.click(screen.getByRole("button", { name: "Add transition" }));
    await user.click(
      screen.getByRole("button", { name: "Replace with Project files" }),
    );
    expect(screen.queryByText("Missing dependency")).toBeNull();
    expect(screen.getByRole("button", { name: "Run" })).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Run" })).toBeEnabled(),
    );
    expect(screen.getByText(/tool\.files/)).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Run" }));
    expect(
      await screen.findByRole("heading", { name: "New Chat" }),
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
    await screen.findByRole("heading", { name: "Repository Engineer" });
    await user.click(screen.getByRole("button", { name: "Release readiness" }));
    expect(
      await screen.findByRole("textbox", { name: "Chat input" }),
    ).toHaveValue("keep this local draft");
  });

  it("preserves a complete unsaved settings draft across route handoff", async () => {
    const user = userEvent.setup();
    render(<App adapters={defaultDesktopAdapters} />);
    await user.click(screen.getByRole("button", { name: /Settings/ }));
    await screen.findByRole("heading", { name: "Settings" });
    const localModel = screen.getByRole("checkbox", { name: "Enabled" });
    expect(localModel).toBeChecked();
    await user.click(localModel);
    expect(screen.getByRole("button", { name: "Save changes" })).toBeEnabled();
    await user.click(screen.getByRole("button", { name: "Release readiness" }));
    await screen.findByRole("heading", { name: "Release readiness" });
    await user.click(screen.getByRole("button", { name: /Settings/ }));
    expect(screen.getByRole("checkbox", { name: "Enabled" })).not.toBeChecked();
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
