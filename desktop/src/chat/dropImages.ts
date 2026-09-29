/**
 * Image files an OS drag-and-drop carries onto the composer.
 *
 * The native window drag-drop interception is disabled, so the webview receives
 * ordinary HTML5 drop events and `dataTransfer.files` holds the dropped files on
 * both Linux (WebKitGTK) and Windows (WebView2). The drop is only accepted when
 * the transfer advertises files: an internal drag (for example the Workflow
 * designer's palette node) must keep its own drop target.
 */
export interface DropDataTransfer {
  readonly types: ArrayLike<string>;
  readonly files: ArrayLike<File>;
}

/** Formats the shared image import path stores; anything else is rejected there. */
const storedImageName = /\.(png|jpe?g|webp)$/i;

function isImageCandidate(file: File): boolean {
  return file.type.startsWith("image/") || storedImageName.test(file.name);
}

/**
 * Whether this drag may carry files, so the composer can claim the drop.
 *
 * Some renderers hide the format list while a drag is over a drop target, so an
 * empty list is claimed permissively; the drop itself then decides by looking at
 * the files it actually received. A non-empty list must name `Files`, which
 * keeps an internal drag (for example the Workflow designer's palette node) on
 * its own drop target.
 */
export function dragCarriesFiles(transfer: DropDataTransfer | null): boolean {
  if (transfer === null) return false;
  if (transfer.types.length === 0) return true;
  for (let index = 0; index < transfer.types.length; index += 1) {
    if (transfer.types[index]?.toLowerCase() === "files") return true;
  }
  return false;
}

/**
 * The image files a drop carries, in the platform's own order.
 *
 * A dropped file the app cannot store is left to the import path so it reports
 * the documented "Choose PNG, JPEG or WebP images." reason rather than the drop
 * appearing to do nothing.
 */
export function droppedImageFiles(
  transfer: DropDataTransfer | null,
): File[] {
  if (transfer === null) return [];
  const files: File[] = [];
  for (let index = 0; index < transfer.files.length; index += 1) {
    const file = transfer.files[index];
    if (file !== undefined && isImageCandidate(file)) files.push(file);
  }
  return files;
}
