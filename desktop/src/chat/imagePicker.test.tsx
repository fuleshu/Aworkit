// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ChatComposer } from "./ChatComposer";
import { chatProjection } from "../test/fixtures/chat";

const native = vi.hoisted(() => ({
  pickChatImages: vi.fn(),
  importChatImage: vi.fn(),
}));

vi.mock("./images", async (original) => ({
  ...(await original<typeof import("./images")>()),
  pickChatImages: native.pickChatImages,
  importChatImage: native.importChatImage,
  chatImagePreview: vi.fn(async () => "data:image/png;base64,aGVsbG8="),
}));

afterEach(() => {
  cleanup();
  native.pickChatImages.mockReset();
  native.importChatImage.mockReset();
});

const stored = {
  id: "a".repeat(64),
  name: "photo.png",
  mimeType: "image/png" as const,
  byteLength: 12,
};

function composer() {
  return render(
    <ChatComposer
      chat={chatProjection({ title: "New Chat", phase: "draft" })}
      projects={[]}
      stale={false}
      pending={false}
      nextCommandId={() => "add-image"}
      onSubmit={vi.fn()}
    />,
  );
}

function openAddImage() {
  fireEvent.click(screen.getByRole("button", { name: "Add attachment" }));
  fireEvent.click(screen.getByRole("menuitem", { name: "Add image" }));
}

describe("the composer's image chooser", () => {
  it("attaches what the operating system's chooser already stored", async () => {
    native.pickChatImages.mockResolvedValue([stored]);
    composer();

    openAddImage();

    expect(await screen.findByRole("img", { name: "photo.png" })).toBeVisible();
    // The native chooser returns stored records, so nothing is read again and
    // the webview file input is never opened.
    expect(native.importChatImage).not.toHaveBeenCalled();
  });

  it("reports a chooser failure next to the composer", async () => {
    native.pickChatImages.mockRejectedValue(
      new Error("Cannot read /work/notes.txt"),
    );
    composer();

    openAddImage();

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Cannot read /work/notes.txt",
    );
  });

  it("keeps the webview input where no native chooser exists", async () => {
    native.pickChatImages.mockResolvedValue(null);
    native.importChatImage.mockResolvedValue(stored);
    composer();

    openAddImage();
    fireEvent.change(screen.getByLabelText("Choose images"), {
      target: { files: [new File(["image"], "photo.png", { type: "image/png" })] },
    });

    expect(await screen.findByRole("img", { name: "photo.png" })).toBeVisible();
    expect(native.importChatImage).toHaveBeenCalledOnce();
  });
});
