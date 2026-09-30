import { useEffect, useRef, useState } from "react";
import { imageAttachmentsSchema, type ImageAttachment } from "./images";

/** The native announcement after a drop; the Rust side owns the import. */
export const DROPPED_IMAGES_EVENT = "aworkit:dropped-images";

/**
 * Whether a dropped path names a file the drop surface accepts.
 *
 * Mirrors the native filter exactly, including the rule that a leading dot is
 * part of the name rather than an extension (`.png` is not a PNG). The
 * announcement only ever carries accepted files, and the hover highlight must
 * not promise to attach a folder or a document.
 */
export function isImagePath(path: string): boolean {
  const leaf = path.slice(
    Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\")) + 1,
  );
  const dot = leaf.lastIndexOf(".");
  return dot > 0 && /^(png|jpe?g|webp)$/i.test(leaf.slice(dot + 1));
}

interface DroppedImagesPayload {
  readonly attachments?: unknown;
  readonly reason?: string | null;
}

/**
 * Attaches image files dropped onto the window.
 *
 * WebKitGTK 2.52 denies the page access to a dropped file's data (WebKit bug
 * 323277), so on Linux the window's own drag-drop event carries the paths and
 * the image is stored natively before it is announced here. The other platforms
 * have no such announcement and keep the composer's HTML5 drop, so this
 * listener is the same everywhere: it either fires or it does not. It also
 * drives the drop highlight, which the denied HTML5 path can no longer raise.
 */
export function useDroppedImages({
  onImages,
  onError,
}: {
  readonly onImages: (images: readonly ImageAttachment[]) => void;
  readonly onError: (message: string) => void;
}): boolean {
  const [dragging, setDragging] = useState(false);
  // The callbacks change on every render; the listener is installed once.
  const handlers = useRef({ onImages, onError });
  handlers.current = { onImages, onError };
  useEffect(() => {
    if (!("__TAURI_INTERNALS__" in window)) return;
    let cancelled = false;
    let stop: (() => void) | null = null;
    void (async () => {
      const [{ getCurrentWebview }, { listen }] = await Promise.all([
        import("@tauri-apps/api/webview"),
        import("@tauri-apps/api/event"),
      ]);
      const stopDrag = await getCurrentWebview().onDragDropEvent((event) => {
        const payload = event.payload;
        if (payload.type === "enter") {
          setDragging(payload.paths.some(isImagePath));
        } else if (payload.type === "drop" || payload.type === "leave") {
          setDragging(false);
        }
      });
      const stopDropped = await listen<DroppedImagesPayload>(
        DROPPED_IMAGES_EVENT,
        ({ payload }) => {
          const attachments = imageAttachmentsSchema.safeParse(
            payload?.attachments,
          );
          if (attachments.success && attachments.data.length > 0) {
            handlers.current.onImages(attachments.data);
            return;
          }
          const reason = payload?.reason ?? null;
          if (reason !== null) handlers.current.onError(reason);
        },
      );
      if (cancelled) {
        stopDrag();
        stopDropped();
        return;
      }
      stop = () => {
        stopDrag();
        stopDropped();
      };
    })().catch(() => {
      /* A browser preview has no native drop surface. */
    });
    return () => {
      cancelled = true;
      stop?.();
    };
  }, []);
  return dragging;
}
