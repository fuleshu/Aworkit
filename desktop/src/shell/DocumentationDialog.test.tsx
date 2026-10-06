// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { DocumentationDialog } from "./DocumentationDialog";

vi.mock("@tauri-apps/api/core", () => ({ isTauri: () => true }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => undefined) }));

// jsdom has no native modal; the dialog only needs the open state for tests.
beforeAll(() => {
  HTMLDialogElement.prototype.showModal = function showModal() {
    this.setAttribute("open", "");
  };
});

afterEach(cleanup);

function openHome(): void {
  render(<DocumentationDialog onClose={vi.fn()} />);
}

/** The dialog's accessible name is its current document title. */
function dialog(): HTMLElement {
  return screen.getByRole("dialog");
}

describe("DocumentationDialog", () => {
  it("opens on the home document with only Home enabled", () => {
    openHome();

    expect(dialog()).toHaveAccessibleName("Aworkit documentation");
    expect(screen.getByRole("button", { name: "Back" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Forward" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Home" })).toBeEnabled();
    // The body renders the Markdown heading, not a raw document dump.
    expect(
      screen.getByRole("heading", { level: 1, name: "Aworkit documentation" }),
    ).toBeVisible();
  });

  it("follows a cross-document link and walks back, forward and home", () => {
    openHome();

    fireEvent.click(
      screen.getByRole("link", {
        name: "Getting started: providers and model tiers",
      }),
    );
    expect(dialog()).toHaveAccessibleName(
      "Getting started: providers and model tiers",
    );
    expect(screen.getByRole("button", { name: "Back" })).toBeEnabled();

    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(dialog()).toHaveAccessibleName("Aworkit documentation");
    expect(screen.getByRole("button", { name: "Forward" })).toBeEnabled();

    fireEvent.click(screen.getByRole("button", { name: "Forward" }));
    expect(dialog()).toHaveAccessibleName(
      "Getting started: providers and model tiers",
    );

    fireEvent.click(screen.getByRole("button", { name: "Home" }));
    expect(dialog()).toHaveAccessibleName("Aworkit documentation");
    expect(screen.getByRole("button", { name: "Forward" })).toBeDisabled();
  });

  it("opens an external link through the system browser", async () => {
    const { openUrl } = await import("@tauri-apps/plugin-opener");
    openHome();

    fireEvent.click(
      screen.getByRole("link", { name: "Further reading and deep dives" }),
    );
    fireEvent.click(screen.getByRole("link", { name: "README" }));

    expect(openUrl).toHaveBeenCalledWith(
      "https://github.com/fuleshu/Aworkit/blob/main/README.md",
    );
  });

  it("closes on the Close button", () => {
    const onClose = vi.fn();
    render(<DocumentationDialog onClose={onClose} />);

    fireEvent.click(screen.getByRole("button", { name: "Close" }));

    expect(onClose).toHaveBeenCalledOnce();
  });
});
