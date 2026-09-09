// @vitest-environment jsdom
import { useState } from "react";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeAll, expect, it, vi } from "vitest";
import type { McpServerConfiguration } from "../configuration";
import { McpToolOptions } from "./McpToolOptions";
import { McpServerSetup } from "./McpServerSetup";

afterEach(cleanup);
beforeAll(() => {
  HTMLDialogElement.prototype.showModal = function () { this.setAttribute("open", ""); };
});
const initial: McpServerConfiguration = { id: "fixture", name: "Fixture server", enabled: true, autoConnect: false,
  transport: { transport: "stdio", command: "fixture-mcp", args: [], cwd: null, env: [] },
  tools: [{ name: "write_record", description: "Write a record", enabled: true, inputSchema: { type: "object" },
    options: { instructions: "Custom guidance", approvalMode: "ask_for_approval" } }] };

function setup(serverInitial: McpServerConfiguration = initial) {
  let latest: McpServerConfiguration;
  function Editor() {
    const [server, setServer] = useState<McpServerConfiguration>(serverInitial);
    latest = server;
    return <McpToolOptions server={server} onChange={tools => setServer({ ...server, tools })} />;
  }
  render(<Editor />);
  fireEvent.click(screen.getByText("write_record"));
  return () => latest;
}

it("requires risk confirmation before enabling and preserves the other tool settings", async () => {
  const latest = setup();
  const checkbox = screen.getByRole("checkbox", { name: "Auto approve" });
  fireEvent.click(checkbox);
  const dialog = screen.getByRole("dialog", { name: "Enable Auto approve?" });
  expect(dialog).toHaveTextContent("changes or deletes data");
  expect(dialog).toHaveTextContent("write_record");
  expect(dialog).toHaveTextContent("Fixture server");
  expect(checkbox).not.toBeChecked();
  expect(latest().tools![0].options?.autoApprove).toBeUndefined();
  fireEvent.click(within(dialog).getByRole("button", { name: "Confirm" }));
  await waitFor(() => expect(checkbox).toBeChecked());
  expect(latest().tools![0].options).toEqual({ ...initial.tools![0].options, autoApprove: true });
  expect(screen.getByRole("combobox", { name: "Approval mode" })).toBeDisabled();
  fireEvent.click(checkbox);
  expect(checkbox).not.toBeChecked();
  expect(latest().tools![0].options).toEqual(initial.tools![0].options);
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  expect(screen.getByRole("combobox", { name: "Approval mode" })).toBeEnabled();
});

it("removes empty default options when disabling Auto approve so native save verification matches", () => {
  const { options: _options, ...tool } = initial.tools![0];
  const server = { ...initial, tools: [tool] };
  const latest = setup(server);
  fireEvent.click(screen.getByRole("checkbox", { name: "Auto approve" }));
  fireEvent.click(screen.getByRole("button", { name: "Confirm" }));
  expect(latest().tools![0].options?.autoApprove).toBe(true);
  fireEvent.click(screen.getByRole("checkbox", { name: "Auto approve" }));
  expect(latest()).toEqual(server);
  expect(Object.hasOwn(latest().tools![0], "options")).toBe(false);
});

it.each(["button", "escape"])("leaves Auto approve off when confirmation is cancelled via %s", method => {
  const latest = setup();
  fireEvent.click(screen.getByRole("checkbox", { name: "Auto approve" }));
  const dialog = screen.getByRole("dialog");
  if (method === "button") fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  else fireEvent(dialog, new Event("cancel", { bubbles: false, cancelable: true }));
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  expect(screen.getByRole("checkbox", { name: "Auto approve" })).not.toBeChecked();
  expect(latest().tools![0].options?.autoApprove).toBeUndefined();
});

it("discards pending confirmation when the server draft changes", () => {
  const onChange = vi.fn();
  const view = render(<McpToolOptions server={initial} onChange={onChange} />);
  fireEvent.click(screen.getByText("write_record"));
  fireEvent.click(screen.getByRole("checkbox", { name: "Auto approve" }));
  view.rerender(<McpToolOptions server={{ ...initial, name: "Changed server" }} onChange={onChange} />);
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  view.rerender(<McpToolOptions server={initial} onChange={onChange} />);
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  expect(onChange).not.toHaveBeenCalled();
});

it("refreshes annotations while preserving the saved Auto approve choice", async () => {
  const onChange = vi.fn();
  const server = { ...initial, tools: initial.tools!.map(tool => ({ ...tool, options: { ...tool.options, autoApprove: true } })) };
  const annotations = { readOnlyHint: false, destructiveHint: true };
  render(<McpServerSetup server={server} onChange={onChange}
    onProbe={async () => ({ ok: true, message: "Connected", draftFingerprint: "fixture", tools: [{ ...initial.tools![0], options: undefined, annotations }] })} />);
  fireEvent.click(screen.getByRole("button", { name: "Refresh functions" }));
  await waitFor(() => expect(onChange).toHaveBeenCalled());
  expect(onChange.mock.calls[0][0].tools[0]).toEqual({ ...server.tools[0], annotations });
});
