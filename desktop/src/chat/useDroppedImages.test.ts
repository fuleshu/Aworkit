// @vitest-environment jsdom
import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { isImagePath, useDroppedImages } from "./useDroppedImages";

type DragHandler = (event: {
  payload: { type: string; paths?: readonly string[] };
}) => void;
type ImagesHandler = (event: {
  payload: { attachments?: unknown; reason?: string | null };
}) => void;

const native = vi.hoisted(() => {
  const state = {
    drag: null as unknown as DragHandler | null,
    images: null as unknown as ImagesHandler | null,
  };
  return {
    state,
    onDragDropEvent: vi.fn(async (handler: DragHandler) => {
      state.drag = handler;
      return () => undefined;
    }),
    listen: vi.fn(async (_event: string, handler: ImagesHandler) => {
      state.images = handler;
      return () => undefined;
    }),
  };
});

vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({ onDragDropEvent: native.onDragDropEvent }),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: native.listen }));

const stored = {
  id: "a".repeat(64),
  name: "photo.png",
  mimeType: "image/png",
  byteLength: 12,
};

beforeEach(() => {
  native.state.drag = null;
  native.state.images = null;
  native.onDragDropEvent.mockClear();
  native.listen.mockClear();
  Object.defineProperty(window, "__TAURI_INTERNALS__", {
    configurable: true,
    value: {},
  });
});

afterEach(() => {
  Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
});

describe("native window drops", () => {
  it("recognizes the files the native drop can import", () => {
    expect(isImagePath("/home/me/photo.png")).toBe(true);
    expect(isImagePath("C:\\Pictures\\holiday.JPEG")).toBe(true);
    expect(isImagePath("/home/me/drawing.webp")).toBe(true);
    expect(isImagePath("/home/me/notes.txt")).toBe(false);
    expect(isImagePath("/home/me/photos")).toBe(false);
    expect(isImagePath("/home/me/.png")).toBe(false);
  });

  it("attaches the images the native import announced", async () => {
    const onImages = vi.fn();
    const onError = vi.fn();
    renderHook(() => useDroppedImages({ onImages, onError }));
    await waitFor(() => expect(native.state.images).not.toBeNull());

    act(() => {
      native.state.images?.({ payload: { attachments: [stored], reason: null } });
    });

    expect(onImages).toHaveBeenCalledWith([stored]);
    expect(onError).not.toHaveBeenCalled();
  });

  it("reports why a drop attached nothing", async () => {
    const onImages = vi.fn();
    const onError = vi.fn();
    renderHook(() => useDroppedImages({ onImages, onError }));
    await waitFor(() => expect(native.state.images).not.toBeNull());

    act(() => {
      native.state.images?.({
        payload: { attachments: [], reason: "Choose PNG, JPEG or WebP images." },
      });
    });

    expect(onImages).not.toHaveBeenCalled();
    expect(onError).toHaveBeenCalledWith("Choose PNG, JPEG or WebP images.");
  });

  it("highlights the drop target from the native drag events", async () => {
    const { result } = renderHook(() =>
      useDroppedImages({ onImages: vi.fn(), onError: vi.fn() }),
    );
    await waitFor(() => expect(native.state.drag).not.toBeNull());

    act(() => {
      native.state.drag?.({ payload: { type: "enter", paths: ["/home/me/photo.png"] } });
    });
    expect(result.current).toBe(true);

    // A drop carrying no image never promises to attach one.
    act(() => {
      native.state.drag?.({ payload: { type: "leave" } });
    });
    expect(result.current).toBe(false);

    act(() => {
      native.state.drag?.({ payload: { type: "enter", paths: ["/home/me/notes.txt"] } });
    });
    expect(result.current).toBe(false);

    act(() => {
      native.state.drag?.({ payload: { type: "enter", paths: ["/home/me/photo.png"] } });
    });
    act(() => {
      native.state.drag?.({ payload: { type: "drop", paths: ["/home/me/photo.png"] } });
    });
    expect(result.current).toBe(false);
  });

  it("installs nothing where there is no native drop surface", () => {
    Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
    const { result } = renderHook(() =>
      useDroppedImages({ onImages: vi.fn(), onError: vi.fn() }),
    );
    expect(result.current).toBe(false);
    expect(native.onDragDropEvent).not.toHaveBeenCalled();
    expect(native.listen).not.toHaveBeenCalled();
  });
});
