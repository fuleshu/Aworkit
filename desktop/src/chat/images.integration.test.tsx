// @vitest-environment jsdom
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ChatComposer } from "./ChatComposer";
import { TimelineCard } from "./ConversationTimeline";
import { toConversationCard } from "./conversation";
import { projectSemanticTimeline } from "./activityProjection";
import { importChatImage, type ImageAttachment } from "./images";
import type { ChatProjection } from "./types";

vi.mock("./images", async (original) => ({
  ...(await original<typeof import("./images")>()),
  importChatImage: vi.fn(async (file: File) => ({
    id: "a".repeat(64),
    name: file.name,
    mimeType: "image/png",
    byteLength: file.size,
  })),
  chatImagePreview: vi.fn(async () => "data:image/png;base64,aGVsbG8="),
}));
const chat: ChatProjection = {
  chatId: "chat.test",
  runId: "run.test",
  title: "New Chat",
  scope: "No project",
  workflowId: null,
  workflowName: null,
  branch: null,
  projectId: null,
  phase: "draft",
  lockedWorkflow: false,
  recoveryPending: false,
  queuedInputs: [],
  expectedVersion: 0,
};
const png = (name: string) => new File(["image"], name, { type: "image/png" });
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("Chat image input", () => {
  it("browses multiple images, previews them and retains an exact failed submission for retry", async () => {
    const submit = vi
      .fn()
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce(true);
    const commandId = vi.fn(() => "image.command");
    render(
      <ChatComposer
        chat={chat}
        projects={[]}
        stale={false}
        pending={false}
        nextCommandId={commandId}
        onSubmit={submit}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Add attachment" }));
    expect(
      screen.getByRole("menuitem", { name: "Add image" }),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("menuitem", { name: "Add image" }));
    fireEvent.change(screen.getByLabelText("Choose images"), {
      target: { files: [png("first.png"), png("second.png")] },
    });
    await screen.findByRole("img", { name: "first.png" });
    await screen.findByRole("img", { name: "second.png" });
    fireEvent.change(screen.getByLabelText("Chat input"), {
      target: { value: "Compare these" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(submit).toHaveBeenCalledTimes(1));
    expect(submit.mock.calls[0][0]).toMatchObject({
      type: "start",
      input: "Compare these",
      attachments: [{ name: "first.png" }, { name: "second.png" }],
    });
    expect(screen.getByRole("img", { name: "first.png" })).toBeInTheDocument();
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Send" })).toBeEnabled(),
    );
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() =>
      expect(
        screen.queryByRole("img", { name: "first.png" }),
      ).not.toBeInTheDocument(),
    );
    expect(submit.mock.calls[1][0]).toEqual(submit.mock.calls[0][0]);
    expect(commandId).toHaveBeenCalledTimes(1);
  });

  it("pastes an image-only follow-up and allows removing individual attachments", async () => {
    const submit = vi.fn(async () => true);
    render(
      <ChatComposer
        chat={{
          ...chat,
          phase: "waiting_input",
          lockedWorkflow: true,
          workflowId: "workflow.simple-chat",
        }}
        projects={[]}
        stale={false}
        pending={false}
        nextCommandId={() => "paste"}
        onSubmit={submit}
      />,
    );
    fireEvent.paste(screen.getByLabelText("Chat input"), {
      clipboardData: {
        files: [png("paste.png"), png("remove.png")],
        getData: () => "",
      },
    });
    await screen.findByRole("img", { name: "paste.png" });
    fireEvent.click(screen.getByRole("button", { name: "Remove remove.png" }));
    fireEvent.click(screen.getByRole("button", { name: "Queue" }));
    await waitFor(() =>
      expect(submit).toHaveBeenCalledWith(
        expect.objectContaining({
          type: "enqueue",
          input: "",
          attachments: [expect.objectContaining({ name: "paste.png" })],
        }),
      ),
    );
  });

  it("preserves ordinary text paste and reports import errors without losing the draft", async () => {
    vi.mocked(importChatImage).mockRejectedValueOnce(
      new Error("Image is corrupt"),
    );
    render(
      <ChatComposer
        chat={chat}
        projects={[]}
        stale={false}
        pending={false}
        nextCommandId={() => "error"}
        onSubmit={vi.fn()}
      />,
    );
    const input = screen.getByLabelText("Chat input");
    fireEvent.change(input, { target: { value: "Keep this" } });
    expect(
      fireEvent.paste(input, {
        clipboardData: { files: [], getData: () => "text" },
      }),
    ).toBe(true);
    fireEvent.change(screen.getByLabelText("Choose images"), {
      target: { files: [png("broken.png")] },
    });
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Image is corrupt",
    );
    expect(input).toHaveValue("Keep this");
  });

  it("projects saved image references into the user message thumbnail", async () => {
    const image: ImageAttachment = {
      id: "a".repeat(64),
      name: "saved.png",
      mimeType: "image/png",
      byteLength: 5,
    };
    const items = projectSemanticTimeline([
      {
        schemaVersion: 1,
        streamId: "chat.test",
        branchId: "main",
        sequence: 1,
        eventId: "image.saved",
        kind: "message.user",
        payload: { body: "", attachments: [image], createdAt: "now" },
      },
    ]);
    const item = items[0];
    expect(item.attachments).toEqual([image]);
    render(
      <TimelineCard
        item={item}
        card={toConversationCard(item)}
        selected={false}
        onSelect={vi.fn()}
        onAction={vi.fn()}
      />,
    );
    expect(
      await screen.findByRole("img", { name: "saved.png" }),
    ).toBeInTheDocument();
  });
});
