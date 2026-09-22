// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { QuestionDialog } from "./QuestionDialog";
import type { QuestionModel } from "./question";

beforeAll(() => {
  HTMLDialogElement.prototype.showModal = function showModal() {
    this.setAttribute("open", "");
  };
});

afterEach(cleanup);

const choice: QuestionModel = {
  questionId: "question.1",
  kind: "choice",
  prompt: "Which release channel should this build target?",
  title: "Release channel",
  options: [
    { id: "stable", label: "Stable" },
    { id: "beta", label: "Beta", description: "Early access" },
  ],
  allowFreeText: true,
  extensions: [],
};

const folder: QuestionModel = {
  questionId: "question.2",
  kind: "folder",
  prompt: "Which folder holds the exported reports?",
  options: [],
  allowFreeText: false,
  extensions: [],
};

describe("question dialog", () => {
  it("is a labelled modal that offers the committed options", async () => {
    const onAnswer = vi.fn();
    render(
      <QuestionDialog
        question={choice}
        busy={false}
        error={null}
        onAnswer={onAnswer}
        onDismiss={vi.fn()}
      />,
    );
    const dialog = screen.getByRole("dialog", { name: "Release channel" });
    expect(dialog).toHaveAttribute("open");
    expect(
      screen.getByText("Which release channel should this build target?"),
    ).toBeVisible();
    const options = screen.getAllByRole("radio");
    expect(options.map((option) => (option as HTMLInputElement).value)).toEqual([
      "stable",
      "beta",
    ]);
    expect(screen.getByText("Early access")).toBeVisible();
    // Nothing is answered until the user submits.
    expect(onAnswer).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Submit answer" })).toBeDisabled();
  });

  it("submits the chosen option with the user's own words", async () => {
    const user = userEvent.setup();
    const onAnswer = vi.fn();
    const { rerender } = render(
      <QuestionDialog
        question={choice}
        busy={false}
        error={null}
        onAnswer={onAnswer}
        onDismiss={vi.fn()}
      />,
    );
    await user.click(screen.getByRole("radio", { name: /Beta/ }));
    await user.type(
      screen.getByRole("textbox", { name: "Your own answer" }),
      "start with beta",
    );
    await user.click(screen.getByRole("button", { name: "Submit answer" }));
    expect(onAnswer).toHaveBeenCalledWith({
      optionId: "beta",
      freeText: "start with beta",
    });

    // A declared default is preselected but never submitted on its own.
    onAnswer.mockClear();
    rerender(
      <QuestionDialog
        key="declared-default"
        question={{ ...choice, defaultOptionId: "beta" }}
        busy={false}
        error={null}
        onAnswer={onAnswer}
        onDismiss={vi.fn()}
      />,
    );
    expect(screen.getByRole("radio", { name: /Beta/ })).toBeChecked();
    expect(onAnswer).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Submit answer" }));
    expect(onAnswer).toHaveBeenCalledWith({ optionId: "beta" });
  });

  it("skips a question as an ordinary result", async () => {
    const user = userEvent.setup();
    const onAnswer = vi.fn();
    render(
      <QuestionDialog
        question={choice}
        busy={false}
        error={null}
        onAnswer={onAnswer}
        onDismiss={vi.fn()}
      />,
    );
    await user.click(screen.getByRole("button", { name: "Skip" }));
    expect(onAnswer).toHaveBeenCalledWith({ cancelled: true });
  });

  it("leaves the question unanswered when the user decides later", async () => {
    const user = userEvent.setup();
    const onAnswer = vi.fn();
    const onDismiss = vi.fn();
    render(
      <QuestionDialog
        question={choice}
        busy={false}
        error={null}
        onAnswer={onAnswer}
        onDismiss={onDismiss}
      />,
    );
    await user.click(screen.getByRole("button", { name: "Decide later" }));
    expect(onDismiss).toHaveBeenCalledTimes(1);
    expect(onAnswer).not.toHaveBeenCalled();
  });

  it("opens the operating system chooser for a path question", async () => {
    const user = userEvent.setup();
    const onAnswer = vi.fn();
    const pickPath = vi.fn().mockResolvedValue("/home/user/reports");
    const { rerender } = render(
      <QuestionDialog
        question={folder}
        busy={false}
        error={null}
        pickPath={pickPath}
        onAnswer={onAnswer}
        onDismiss={vi.fn()}
      />,
    );
    expect(screen.getByText("No folder chosen yet.")).toBeVisible();
    expect(screen.getByRole("button", { name: "Submit answer" })).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "Choose folder…" }));
    expect(pickPath).toHaveBeenCalledWith("folder", []);
    await waitFor(() =>
      expect(screen.getByText("/home/user/reports")).toBeVisible(),
    );
    await user.click(screen.getByRole("button", { name: "Submit answer" }));
    expect(onAnswer).toHaveBeenCalledWith({ path: "/home/user/reports" });

    // Dismissing the chooser leaves the dialog exactly as it was: the previous
    // choice survives and nothing is submitted.
    const dismissing = vi.fn().mockResolvedValue(null);
    rerender(
      <QuestionDialog
        question={folder}
        busy={false}
        error={null}
        pickPath={dismissing}
        onAnswer={onAnswer}
        onDismiss={vi.fn()}
      />,
    );
    await user.click(screen.getByRole("button", { name: "Choose folder…" }));
    await waitFor(() => expect(dismissing).toHaveBeenCalled());
    expect(screen.getByText("/home/user/reports")).toBeVisible();
    expect(onAnswer).toHaveBeenCalledTimes(1);
  });

  it("offers no choose button when this desktop has no operating system chooser", () => {
    render(
      <QuestionDialog
        question={folder}
        busy={false}
        error={null}
        onAnswer={vi.fn()}
        onDismiss={vi.fn()}
      />,
    );
    expect(screen.getByRole("button", { name: "Choose folder…" })).toBeDisabled();
  });

  it("reports a rejected answer and blocks while it is being sent", () => {
    render(
      <QuestionDialog
        question={choice}
        busy
        error="Question already has a different answer"
        onAnswer={vi.fn()}
        onDismiss={vi.fn()}
      />,
    );
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Question already has a different answer",
    );
    expect(screen.getByRole("button", { name: "Sending…" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Skip" })).toBeDisabled();
  });
});
