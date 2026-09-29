/**
 * Native clipboard image read for renderers that never hand the pasted image to
 * the page.
 *
 * WebKitGTK (Tauri's renderer on Linux) delivers a completely empty
 * `DataTransfer` for a clipboard image: no files, no items, and not even a
 * `types` entry, even though the OS clipboard demonstrably holds `image/png`.
 * The upstream WebKit path (bug 218519) still drops the payload before the web
 * content sees it, so no amount of `clipboardData` inspection can recover it.
 * The OS clipboard does have the image, so the fallback reads it natively
 * through the clipboard plugin and re-encodes it as a PNG File that the shared
 * image import path already accepts.
 *
 * This is only reached when the webview offered nothing at all: Chromium and
 * Windows WebView2 populate the paste data, so their paste uses the direct path
 * and never touches the native clipboard.
 */
const pastedImageName = "Pasted image.png";

/** Whether a native clipboard read is possible in this renderer. */
export function nativeClipboardImageAvailable(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/**
 * The image on the OS clipboard as a PNG File, or null when there is none.
 *
 * A clipboard without an image is an ordinary state, not a failure, so every
 * refusal (no image, unsupported native format, no canvas) resolves to null
 * instead of surfacing an error next to the composer.
 */
export async function readNativeClipboardImage(): Promise<File | null> {
  if (!nativeClipboardImageAvailable()) return null;
  try {
    const { readImage } = await import("@tauri-apps/plugin-clipboard-manager");
    const image = await readImage();
    try {
      const [size, rgba] = await Promise.all([image.size(), image.rgba()]);
      if (
        !Number.isInteger(size.width) ||
        !Number.isInteger(size.height) ||
        size.width <= 0 ||
        size.height <= 0 ||
        rgba.length !== size.width * size.height * 4
      ) {
        return null;
      }
      return await rgbaToPngFile(size.width, size.height, rgba);
    } finally {
      await image.close();
    }
  } catch {
    return null;
  }
}

/** Re-encodes the plugin's raw RGBA buffer as a storeable PNG File. */
async function rgbaToPngFile(
  width: number,
  height: number,
  rgba: Uint8Array,
): Promise<File | null> {
  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  const context = canvas.getContext("2d");
  if (context === null) return null;
  context.putImageData(
    new ImageData(new Uint8ClampedArray(rgba), width, height),
    0,
    0,
  );
  const blob = await new Promise<Blob | null>((resolve) =>
    canvas.toBlob(resolve, "image/png"),
  );
  if (blob === null) return null;
  return new File([blob], pastedImageName, { type: "image/png" });
}
