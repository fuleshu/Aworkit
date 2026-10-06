// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DocumentsExtrasControl } from "./DocumentsExtrasControl";

const port = vi.hoisted(() => ({
  status: vi.fn(),
  setEnabled: vi.fn(),
  writeNow: vi.fn(),
  openFolder: vi.fn(),
}));

vi.mock("../../shell/documentsExtras", () => ({
  documentsExtrasStatus: port.status,
  setDocumentsExtrasEnabled: port.setEnabled,
  writeDocumentsExtras: port.writeNow,
  openDocumentsFolder: port.openFolder,
}));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

const written = {
  enabled: true,
  folder: "C:\\Users\\me\\Documents\\Aworkit",
  exists: true,
  exampleWorkflowCount: 4,
  pluginPresent: true,
  writtenVersion: 1,
} as const;

describe("DocumentsExtrasControl", () => {
  it("shows the folder and its contents when the extras were written", async () => {
    port.status.mockResolvedValue(written);
    render(<DocumentsExtrasControl />);

    expect(await screen.findByText(/Example Workflows|Aworkit/)).toBeTruthy();
    const toggle = screen.getByRole("checkbox", {
      name: "Write them into my documents folder",
    });
    expect(toggle).toBeEnabled();
    expect(toggle).toBeChecked();
    await screen.findByText(/4 example workflows, FFmpeg plugin present/);
  });

  it("turns writing off through the native port", async () => {
    port.status.mockResolvedValue(written);
    port.setEnabled.mockResolvedValue({ ...written, enabled: false });
    render(<DocumentsExtrasControl />);
    const toggle = await screen.findByRole("checkbox", {
      name: "Write them into my documents folder",
    });

    fireEvent.click(toggle);

    await waitFor(() => expect(port.setEnabled).toHaveBeenCalledWith(false));
    await waitFor(() => expect(toggle).not.toBeChecked());
  });

  it("writes missing files and reveals the folder on demand", async () => {
    port.status.mockResolvedValue(written);
    port.writeNow.mockResolvedValue(written);
    port.openFolder.mockResolvedValue(undefined);
    render(<DocumentsExtrasControl />);
    await screen.findByRole("checkbox", { name: "Write them into my documents folder" });

    fireEvent.click(screen.getByRole("button", { name: "Write now" }));
    await waitFor(() => expect(port.writeNow).toHaveBeenCalledOnce());

    fireEvent.click(screen.getByRole("button", { name: "Open folder" }));
    await waitFor(() => expect(port.openFolder).toHaveBeenCalledOnce());
  });

  it("stays inert in a browser preview without a native host", async () => {
    port.status.mockResolvedValue(null);
    render(<DocumentsExtrasControl />);

    expect(await screen.findByText("Available in the desktop app.")).toBeVisible();
    expect(
      screen.getByRole("checkbox", { name: "Write them into my documents folder" }),
    ).toBeDisabled();
  });
});
