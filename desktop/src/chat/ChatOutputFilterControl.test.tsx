// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ChatOutputFilter } from "./ChatOutputFilterControl";

afterEach(cleanup);

describe("ChatOutputFilter", () => {
  it("opens the filter dialog in a portal and reports a checkbox change", async () => {
    const onChange = vi.fn();
    const user = userEvent.setup();
    render(
      <ChatOutputFilter
        value={{ thinking: true, tools: true }}
        onChange={onChange}
      />,
    );
    const trigger = screen.getByRole("button", { name: "Chat output filter" });
    expect(trigger).toHaveAttribute("aria-expanded", "false");
    expect(
      screen.queryByRole("dialog", { name: "Chat output filter" }),
    ).toBeNull();

    await user.click(trigger);
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    expect(
      screen.getByRole("dialog", { name: "Chat output filter" }),
    ).toBeVisible();

    await user.click(screen.getByRole("checkbox", { name: "Thinking" }));
    expect(onChange).toHaveBeenCalledWith({ thinking: false, tools: true });
  });

  it("names the hidden kinds in its accessible name", () => {
    render(
      <ChatOutputFilter
        value={{ thinking: false, tools: true }}
        onChange={vi.fn()}
      />,
    );
    expect(
      screen.getByRole("button", {
        name: "Chat output filter (hiding reasoning)",
      }),
    ).toBeVisible();
  });

  it("stays open while the transcript scrolls during a Run", async () => {
    const user = userEvent.setup();
    render(
      <ChatOutputFilter
        value={{ thinking: true, tools: true }}
        onChange={vi.fn()}
      />,
    );
    await user.click(screen.getByRole("button", { name: "Chat output filter" }));
    const dialog = screen.getByRole("dialog", { name: "Chat output filter" });
    expect(dialog).toBeVisible();

    // Streaming content scrolls the timeline; the panel must survive that.
    act(() => {
      window.dispatchEvent(new Event("scroll"));
      document.dispatchEvent(new Event("scroll", { bubbles: true }));
    });
    expect(
      screen.getByRole("dialog", { name: "Chat output filter" }),
    ).toBeVisible();
  });
});
