// @vitest-environment jsdom
import { render, screen, cleanup } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
afterEach(cleanup);
import { ApprovalActions } from "./ApprovalActions";
import { timelineActionIntent } from "./ChatWorkspaceScreen";
import { chatIntentPayload } from "./corePort";

describe("Approval decisions", () => {
  it("sends exact project and one-use choices through the native payload", async () => {
    const user = userEvent.setup();
    const onDecision = vi.fn();
    render(<ApprovalActions disabled={false} projectScope="Files in this project" onDecision={onDecision} />);
    await user.click(screen.getByRole("button", { name: "Always approve in project" }));
    const choice = onDecision.mock.calls[0]![0];
    expect(chatIntentPayload(timelineActionIntent("approve", "decision.exact", "command.exact", choice))).toEqual({ decisionId: "decision.exact", approved: true, choice: "always_approve_in_project" });
    await user.click(screen.getByRole("button", { name: "Approve once" }));
    expect(onDecision.mock.calls[1]![0]).toEqual({ choice: "approve_once" });
  });

  it("requires a denial reason and keeps unscoped project approval disabled", async () => {
    const user = userEvent.setup(); const onDecision = vi.fn();
    render(<ApprovalActions disabled={false} onDecision={onDecision} />);
    expect(screen.getByRole("button", { name: "Always approve in project" })).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "Deny and give reason" }));
    expect(screen.getByRole("button", { name: "Deny action" })).toBeDisabled();
    await user.type(screen.getByRole("textbox", { name: "Reason for denial" }), " Keep it unchanged. ");
    await user.click(screen.getByRole("button", { name: "Deny action" }));
    const details = onDecision.mock.calls[0]![0];
    expect(chatIntentPayload(timelineActionIntent("reject", "decision.1", "command.1", details))).toEqual({ decisionId: "decision.1", approved: false, choice: "deny", reason: "Keep it unchanged." });
  });
});
