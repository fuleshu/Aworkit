// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import {
  ABOUT_LIBRARIES,
  ABOUT_SOURCE_URL,
  ABOUT_WEBPAGE_URL,
  AboutDialog,
} from "./AboutDialog";

vi.mock("@tauri-apps/api/app", () => ({
  getVersion: vi.fn(async () => "9.9.9"),
}));
vi.mock("@tauri-apps/api/core", () => ({ isTauri: () => false }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));

// jsdom has no native modal; the dialog only needs the open state for tests.
beforeAll(() => {
  HTMLDialogElement.prototype.showModal = function showModal() {
    this.setAttribute("open", "");
  };
});

afterEach(cleanup);

describe("AboutDialog", () => {
  it("credits the developer, company, links, license and libraries", async () => {
    render(<AboutDialog onClose={vi.fn()} />);

    expect(
      screen.getByRole("heading", { name: "About Aworkit" }),
    ).toBeVisible();
    expect(await screen.findByText("Version 9.9.9")).toBeVisible();
    expect(screen.getByText("Timo Fleisch")).toBeVisible();
    expect(screen.getByText("d.b.a. klutzGames")).toBeVisible();
    expect(screen.getByRole("link", { name: ABOUT_WEBPAGE_URL })).toHaveAttribute(
      "href",
      ABOUT_WEBPAGE_URL,
    );
    expect(screen.getByRole("link", { name: ABOUT_SOURCE_URL })).toHaveAttribute(
      "href",
      ABOUT_SOURCE_URL,
    );
    expect(screen.getAllByText("Apache-2.0").length).toBeGreaterThan(0);

    expect(
      screen.getByRole("heading", { name: "Third-party libraries" }),
    ).toBeVisible();
    for (const library of ABOUT_LIBRARIES) {
      expect(screen.getByText(library.name)).toBeVisible();
    }
  });

  it("closes on the Close button", () => {
    const onClose = vi.fn();
    render(<AboutDialog onClose={onClose} />);

    fireEvent.click(screen.getByRole("button", { name: "Close" }));

    expect(onClose).toHaveBeenCalledOnce();
  });

  it("closes when the modal is cancelled", () => {
    const onClose = vi.fn();
    render(<AboutDialog onClose={onClose} />);

    fireEvent(
      screen.getByRole("dialog"),
      new Event("cancel", { cancelable: true }),
    );

    expect(onClose).toHaveBeenCalledOnce();
  });
});
