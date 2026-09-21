// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SubagentViewSection } from "./SubagentViewSection";
import type { SubagentViewPreferencePort } from "../../chat/subagentViewPreference";

afterEach(cleanup);

describe("Settings subagent tab preference", () => {
  it("loads the stored preference and commits each change through its own command", async () => {
    const user = userEvent.setup();
    const commit = vi.fn().mockResolvedValue(undefined);
    const snapshot = vi
      .fn()
      .mockResolvedValueOnce({ autoOpen: false, autoClose: true, version: 4 })
      .mockResolvedValue({ autoOpen: true, autoClose: true, version: 5 });
    const port: SubagentViewPreferencePort = { snapshot, commit };
    const onChange = vi.fn();
    render(<SubagentViewSection port={port} onChange={onChange} />);
    const autoOpen = await screen.findByRole("checkbox", {
      name: /Open new subagent tabs/,
    });
    const autoClose = screen.getByRole("checkbox", {
      name: /Close settled subagent tabs/,
    });
    await waitFor(() => expect(autoOpen).not.toBeChecked());
    expect(autoClose).toBeChecked();
    expect(onChange).toHaveBeenLastCalledWith({
      autoOpen: false,
      autoClose: true,
      version: 4,
    });

    await user.click(autoOpen);
    expect(commit).toHaveBeenCalledWith(
      { autoOpen: true, autoClose: true },
      4,
    );
    await waitFor(() => expect(autoOpen).toBeChecked());
    expect(await screen.findByText("Saved")).toBeVisible();
    expect(onChange).toHaveBeenLastCalledWith({
      autoOpen: true,
      autoClose: true,
      version: 5,
    });
  });

  it("restores the committed value when the preference write fails", async () => {
    const user = userEvent.setup();
    const port: SubagentViewPreferencePort = {
      snapshot: vi
        .fn()
        .mockResolvedValue({ autoOpen: true, autoClose: false, version: 2 }),
      commit: vi.fn().mockRejectedValue(new Error("settings are read-only")),
    };
    render(<SubagentViewSection port={port} />);
    const autoClose = await screen.findByRole("checkbox", {
      name: /Close settled subagent tabs/,
    });
    await waitFor(() => expect(autoClose).not.toBeChecked());
    await user.click(autoClose);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "settings are read-only",
    );
    expect(autoClose).not.toBeChecked();
  });
});
