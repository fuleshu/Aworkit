// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { GoalControl } from "./GoalControl";
import type { RuntimeEvent } from "./corePort";
import { runtimeEvent } from "../test/fixtures/chat";

afterEach(cleanup);

const event = (sequence: number, goal: Record<string, unknown>): RuntimeEvent =>
  runtimeEvent(sequence, "tool.goal", { goal }, { streamId: "chat.goal", eventId: `e.${sequence}` });

it("opens the durable goal editable and submits the revised text", async () => {
  const submit = vi.fn().mockResolvedValue(true);
  render(
    <GoalControl
      events={[event(1, { status: "active", goal: "Old objective" })]}
      disabledReason={null}
      onSubmit={submit}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: /Chat goal/ }));
  const field = screen.getByRole("textbox", { name: "Goal" }) as HTMLTextAreaElement;
  expect(field.value).toBe("Old objective");
  fireEvent.change(field, { target: { value: "New objective" } });
  fireEvent.click(screen.getByRole("button", { name: "Submit" }));
  await waitFor(() => expect(submit).toHaveBeenCalledWith("New objective"));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
});

it("clears a live goal and cancels without submitting", async () => {
  const submit = vi.fn().mockResolvedValue(true);
  render(
    <GoalControl
      events={[event(1, { status: "active", goal: "Ship 108" })]}
      disabledReason={null}
      onSubmit={submit}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: /Chat goal/ }));
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  expect(submit).not.toHaveBeenCalled();
  expect(screen.queryByRole("dialog")).toBeNull();

  fireEvent.click(screen.getByRole("button", { name: /Chat goal/ }));
  fireEvent.click(screen.getByRole("button", { name: "Clear goal" }));
  await waitFor(() => expect(submit).toHaveBeenCalledWith(null));
});

it("offers no clear action without a live goal and explains a disabled reason", () => {
  render(<GoalControl events={[]} disabledReason="Resynchronize first." onSubmit={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: /not set/ }));
  expect(screen.queryByRole("button", { name: "Clear goal" })).toBeNull();
  expect(screen.getByText("Resynchronize first.")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Submit" }).hasAttribute("disabled")).toBe(true);
});

it("reports a rejected goal change without closing the dialog", async () => {
  const submit = vi.fn().mockResolvedValue(false);
  render(<GoalControl events={[]} disabledReason={null} onSubmit={submit} />);
  fireEvent.click(screen.getByRole("button", { name: /not set/ }));
  fireEvent.change(screen.getByRole("textbox", { name: "Goal" }), {
    target: { value: "Objective" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Submit" }));
  await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("not committed"));
  expect(screen.queryByRole("dialog")).not.toBeNull();
});
