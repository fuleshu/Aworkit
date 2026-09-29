/**
 * What a paste event's clipboard offers, narrowed to the fields this app reads.
 *
 * `DataTransfer` satisfies it structurally. The narrower shape keeps the reader
 * testable with plain objects and stops it from reaching for anything that is
 * only valid while the paste event is being dispatched.
 */
export interface PasteClipboard {
  /** Files the platform already exposes as paste files. Empty on WebKit. */
  readonly files: ArrayLike<File>;
  /** The clipboard's item list, which is where WebKit puts pasted images. */
  readonly items?: ArrayLike<PasteItem> | undefined;
  /** The offered formats; a WebKitGTK image paste offers none of them at all. */
  readonly types?: ArrayLike<string> | undefined;
  /** Present on a real paste event; absent in plain-object tests. */
  getData?(type: string): string;
}

/** One clipboard item; only its kind, type and file are ever needed. */
export interface PasteItem {
  readonly kind: string;
  readonly type: string;
  getAsFile(): File | null;
}

/** Formats the shared image import path stores; anything else is rejected there. */
const storedImageName = /\.(png|jpe?g|webp)$/i;

/**
 * True for a pasted file that is worth handing to the shared image import path.
 *
 * An image MIME type passes even when the app cannot store it, so the import
 * path reports the user-facing "PNG, JPEG or WebP" reason instead of the paste
 * silently doing nothing. A clipboard entry without a MIME type still counts
 * when its name carries a supported image extension.
 */
function isImageCandidate(file: File): boolean {
  return file.type.startsWith("image/") || storedImageName.test(file.name);
}

/**
 * The image files a paste event carries.
 *
 * WebKit and Chromium disagree about where pasted image data appears. Chromium
 * fills `clipboardData.files`, which is checked first so the common path stays
 * allocation-free and an image never arrives twice. WebKitGTK leaves `files`
 * empty — it is filled from clipboard *file paths*, not from clipboard image
 * data — and puts the pasted image in the item list only, as a `kind: "file"`
 * item whose type is the image MIME type the GTK pasteboard reported
 * (`image/png`, `image/jpeg`, `image/gif`, `image/bmp`, `image/x-icon`), named
 * `image.png` and so on. Without that second read the platform's image never
 * reaches the import path and the paste does nothing at all.
 *
 * The read must stay synchronous: once the paste handler returns, WebKit and
 * Chromium neuter the data transfer and `getAsFile()` yields null.
 */
export function pastedImageFiles(clipboard: PasteClipboard | null): File[] {
  if (clipboard === null) return [];
  const fromFiles: File[] = [];
  for (let index = 0; index < clipboard.files.length; index += 1) {
    const file = clipboard.files[index];
    if (file !== undefined && isImageCandidate(file)) fromFiles.push(file);
  }
  if (fromFiles.length > 0) return fromFiles;
  const items = clipboard.items;
  if (items === undefined) return [];
  const fromItems: File[] = [];
  for (let index = 0; index < items.length; index += 1) {
    const item = items[index];
    if (item === undefined || item.kind !== "file") continue;
    // An image MIME type is the platform's own statement; an empty type falls
    // back to the file name, which WebKit derives from the reported format.
    if (!item.type.startsWith("image/") && item.type !== "") continue;
    const file = item.getAsFile();
    if (file !== null && file.size > 0 && isImageCandidate(file)) {
      fromItems.push(file);
    }
  }
  return fromItems;
}

/** What the composer must do with one paste event. */
export type PasteIntent =
  /** The default paste handles it (text, or a copied non-image file). */
  | { readonly kind: "ignore" }
  /** The webview exposed the pasted image files. */
  | {
      readonly kind: "files";
      readonly files: readonly File[];
      readonly text: string;
    }
  /**
   * The webview exposed nothing at all: on Linux WebKitGTK a clipboard image
   * arrives as a completely empty `DataTransfer` (no files, no items, not even
   * a `types` entry), so the image can only be read from the OS clipboard.
   */
  | { readonly kind: "native" };

function plainText(clipboard: PasteClipboard): string {
  if (clipboard.getData === undefined) return "";
  try {
    return clipboard.getData("text/plain");
  } catch {
    return "";
  }
}

/** A copied file or a text flavour that the default paste already consumes. */
function offersOtherContent(clipboard: PasteClipboard): boolean {
  if (plainText(clipboard) !== "") return true;
  const types = clipboard.types;
  if (types === undefined) return false;
  for (let index = 0; index < types.length; index += 1) {
    const type = types[index];
    if (
      type !== undefined &&
      (type.toLowerCase() === "files" || type.toLowerCase() === "text/uri-list")
    ) {
      return true;
    }
  }
  return false;
}

/**
 * Classify a paste so the composer claims exactly the pastes it can serve.
 *
 * An empty clipboard is reported as `native` so the caller can try the OS
 * clipboard once; a text-only paste is left to the default paste so typing and
 * IME behaviour is untouched.
 */
export function pasteIntent(clipboard: PasteClipboard | null): PasteIntent {
  if (clipboard === null) return { kind: "ignore" };
  const files = pastedImageFiles(clipboard);
  if (files.length > 0)
    return { kind: "files", files, text: plainText(clipboard) };
  if (offersOtherContent(clipboard)) return { kind: "ignore" };
  return { kind: "native" };
}
