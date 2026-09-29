// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { CreateProjectDialog } from "./CreateProjectDialog";
import type { ChatProjectChoice } from "./types";

// jsdom has no native modal; the dialog only needs the open state for tests.
beforeAll(() => {
  HTMLDialogElement.prototype.showModal = function showModal() {
    this.setAttribute("open", "");
  };
});

afterEach(cleanup);

const created: ChatProjectChoice = {
  projectId: "project.abc",
  name: "repo",
  workspaceKind: "local_directory",
};

describe("CreateProjectDialog", () => {
  it("seeds the name from the chosen folder and creates the project", async () => {
    const create = vi.fn(async () => created);
    const onCreated = vi.fn();
    render(
      <CreateProjectDialog
        pickFolder={vi.fn(async () => "/home/me/Projects/repo")}
        create={create}
        onCreated={onCreated}
        onCancel={vi.fn()}
      />,
    );

    expect(
      screen.getByRole("button", { name: "Create project" }),
    ).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Browse…" }));
    expect(await screen.findByDisplayValue("/home/me/Projects/repo")).toBeVisible();
    expect(screen.getByLabelText("Project name")).toHaveValue("repo");

    fireEvent.click(screen.getByRole("button", { name: "Create project" }));
    expect(create).toHaveBeenCalledWith({
      name: "repo",
      kind: "local_directory",
      location: "/home/me/Projects/repo",
    });
    await vi.waitFor(() => expect(onCreated).toHaveBeenCalledWith(created));
  });

  it("keeps a typed name and the workspace kind the user chose", async () => {
    const create = vi.fn(async () => created);
    render(
      <CreateProjectDialog
        pickFolder={vi.fn(async () => "/home/me/Projects/repo")}
        create={create}
        onCreated={vi.fn()}
        onCancel={vi.fn()}
      />,
    );

    fireEvent.change(screen.getByLabelText("Project name"), {
      target: { value: "Release train" },
    });
    fireEvent.change(screen.getByLabelText("Workspace kind"), {
      target: { value: "git_worktree" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Browse…" }));
    expect(await screen.findByDisplayValue("/home/me/Projects/repo")).toBeVisible();
    // A chosen folder never overwrites a name the user typed.
    expect(screen.getByLabelText("Project name")).toHaveValue("Release train");

    fireEvent.click(screen.getByRole("button", { name: "Create project" }));
    expect(create).toHaveBeenCalledWith({
      name: "Release train",
      kind: "git_worktree",
      location: "/home/me/Projects/repo",
    });
  });

  it("shows why a refused save failed and creates nothing", async () => {
    const onCreated = vi.fn();
    render(
      <CreateProjectDialog
        create={vi.fn(async () => {
          throw new Error("settings version conflict: expected 7, actual 8");
        })}
        onCreated={onCreated}
        onCancel={vi.fn()}
      />,
    );

    fireEvent.change(screen.getByLabelText("Project name"), {
      target: { value: "repo" },
    });
    fireEvent.change(screen.getByLabelText("Workspace folder"), {
      target: { value: "/home/me/Projects/repo" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create project" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "settings version conflict",
    );
    expect(onCreated).not.toHaveBeenCalled();
    // The entered values survive so the user can retry.
    expect(screen.getByLabelText("Workspace folder")).toHaveValue(
      "/home/me/Projects/repo",
    );
  });

  it("cancels without creating anything", () => {
    const onCancel = vi.fn();
    const create = vi.fn();
    render(
      <CreateProjectDialog
        create={create}
        onCreated={vi.fn()}
        onCancel={onCancel}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onCancel).toHaveBeenCalledOnce();
    expect(create).not.toHaveBeenCalled();
  });
});
