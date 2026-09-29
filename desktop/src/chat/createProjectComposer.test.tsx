// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { ChatComposer } from "./ChatComposer";
import { chatProjection } from "../test/fixtures/chat";
import type { ChatProjectChoice, ChatProjection } from "./types";

// jsdom has no native modal; the dialog only needs the open state for tests.
beforeAll(() => {
  HTMLDialogElement.prototype.showModal = function showModal() {
    this.setAttribute("open", "");
  };
});

afterEach(cleanup);

const chat: ChatProjection = chatProjection({
  title: "New Chat",
  phase: "draft",
});

const created: ChatProjectChoice = {
  projectId: "project.created",
  name: "repo",
  workspaceKind: "local_directory",
};

describe("creating a project from the composer", () => {
  it("creates the project from the dropdown and selects it immediately", async () => {
    const onCreateProject = vi.fn(async () => created);
    render(
      <ChatComposer
        chat={chat}
        projects={[]}
        stale={false}
        pending={false}
        nextCommandId={() => "create-project"}
        onCreateProject={onCreateProject}
        pickFolder={async () => "/home/me/Projects/repo"}
        onSubmit={vi.fn()}
      />,
    );

    const select = screen.getByLabelText("Project for the first Chat input");
    fireEvent.change(select, { target: { value: "new project" } });
    // Choosing the entry opens the dialog instead of selecting a project id.
    expect(select).toHaveValue("");
    expect(
      screen.getByRole("heading", { name: "Create project" }),
    ).toBeVisible();

    fireEvent.click(screen.getByRole("button", { name: "Browse…" }));
    expect(
      await screen.findByDisplayValue("/home/me/Projects/repo"),
    ).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "Create project" }));

    await vi.waitFor(() => expect(onCreateProject).toHaveBeenCalled());
    expect(onCreateProject).toHaveBeenCalledWith({
      name: "repo",
      kind: "local_directory",
      location: "/home/me/Projects/repo",
    });
    // The new project becomes this Chat's selection before any snapshot repeats
    // it, and its own option is rendered so the value is never dangling.
    expect(select).toHaveValue("project.created");
    expect(
      screen.getByRole("option", { name: "repo" }),
    ).toBeInTheDocument();
  });

  it("leaves the selection untouched when the dialog is cancelled", () => {
    render(
      <ChatComposer
        chat={chat}
        projects={[]}
        stale={false}
        pending={false}
        nextCommandId={() => "create-project"}
        onCreateProject={vi.fn(async () => created)}
        pickFolder={async () => null}
        onSubmit={vi.fn()}
      />,
    );

    fireEvent.change(
      screen.getByLabelText("Project for the first Chat input"),
      { target: { value: "new project" } },
    );
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(
      screen.queryByRole("heading", { name: "Create project" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByLabelText("Project for the first Chat input"),
    ).toHaveValue("");
  });

  it("offers no create entry when the Chat cannot save a project", () => {
    render(
      <ChatComposer
        chat={chat}
        projects={[]}
        stale={false}
        pending={false}
        nextCommandId={() => "create-project"}
        onSubmit={vi.fn()}
      />,
    );
    expect(
      screen.queryByRole("option", { name: "Create New Project…" }),
    ).not.toBeInTheDocument();
  });
});
