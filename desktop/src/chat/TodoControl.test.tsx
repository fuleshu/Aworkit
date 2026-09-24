// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { TodoControl } from "./TodoControl";
import type { RuntimeEvent } from "./corePort";
import { runtimeEvent } from "../test/fixtures/chat";

afterEach(cleanup);

const event = (sequence: number, todos: readonly unknown[]): RuntimeEvent =>
  runtimeEvent(
    sequence,
    "tool.todo",
    { todos },
    { streamId: "chat.todo", eventId: `e.${sequence}` },
  );

const PLANNED = [
  { content: "Write the engine", status: "completed" },
  { content: "Write the renderer", status: "in_progress" },
  { content: "Write the README", status: "pending" },
];

it("shows an explicit empty state before the agent writes a list", () => {
  render(<TodoControl events={[]} />);
  fireEvent.click(screen.getByRole("button", { name: /Task list/ }));
  expect(screen.getByRole("dialog", { name: "Task list" })).toBeTruthy();
  expect(screen.getByText("This Chat has no task list yet.")).toBeTruthy();
  expect(screen.queryByRole("listitem")).toBeNull();
});

it("lists every entry with its status and a completed count", () => {
  render(<TodoControl events={[event(1, PLANNED)]} />);
  const trigger = screen.getByRole("button", {
    name: /Task list \(1 of 3 done\)/,
  });
  expect(trigger.getAttribute("aria-haspopup")).toBe("dialog");
  expect(trigger.getAttribute("aria-expanded")).toBe("false");
  fireEvent.click(trigger);
  expect(trigger.getAttribute("aria-expanded")).toBe("true");
  expect(screen.getByText("1 of 3 done")).toBeTruthy();
  const entries = screen
    .getAllByRole("listitem")
    .map((item) => item.textContent);
  expect(entries).toEqual([
    "CompletedWrite the engine",
    "In progressWrite the renderer",
    "PendingWrite the README",
  ]);
});

it("follows a newer task list that arrives while the run continues", () => {
  const { rerender } = render(<TodoControl events={[event(1, PLANNED)]} />);
  fireEvent.click(screen.getByRole("button", { name: /Task list/ }));
  expect(screen.getByText("1 of 3 done")).toBeTruthy();
  rerender(
    <TodoControl
      events={[
        event(1, PLANNED),
        event(2, [
          { content: "Write the engine", status: "completed" },
          { content: "Write the renderer", status: "completed" },
          { content: "Write the README", status: "completed" },
        ]),
      ]}
    />,
  );
  expect(screen.getByText("3 of 3 done")).toBeTruthy();
});

it("closes on Escape and on a second click, returning focus to the button", () => {
  render(<TodoControl events={[event(1, PLANNED)]} />);
  const trigger = screen.getByRole("button", { name: /Task list/ });
  fireEvent.click(trigger);
  const dialog = screen.getByRole("dialog", { name: "Task list" });
  fireEvent.keyDown(dialog, { key: "Escape" });
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(document.activeElement).toBe(trigger);

  fireEvent.click(trigger);
  expect(screen.queryByRole("dialog")).toBeTruthy();
  fireEvent.click(trigger);
  expect(screen.queryByRole("dialog")).toBeNull();
});

it("treats an unknown status as pending work", () => {
  render(
    <TodoControl
      events={[event(1, [{ content: "Ship it", status: "whatever" }])]}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: /Task list/ }));
  expect(screen.getByText("0 of 1 done")).toBeTruthy();
  expect(screen.getAllByRole("listitem")[0].textContent).toBe("PendingShip it");
});
