import { useEffect, useRef, useState } from "react";
import {
  importChatImage,
  maxImageContextBytes,
  maxImages,
  validateImageSelection,
  type ImageAttachment,
} from "./images";

/** Serializes imports, retains existing attachments on failure, and prevents a
 * slow file read from adding images to a different Chat after navigation. */
export function useChatImages(
  images: readonly ImageAttachment[],
  onChange: (images: readonly ImageAttachment[]) => void,
) {
  const [importing, setImporting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const busy = useRef(false);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  async function addFiles(files: readonly File[]) {
    if (busy.current || files.length === 0) return;
    busy.current = true;
    setImporting(true);
    setError(null);
    try {
      if (
        images.length + files.length > maxImages ||
        images.reduce((sum, image) => sum + image.byteLength, 0) +
          files.reduce((sum, file) => sum + file.size, 0) >
          maxImageContextBytes
      )
        throw new Error("Add up to 20 images, totalling at most 12 MiB.");
      const added: ImageAttachment[] = [];
      for (const file of files) {
        if (!mounted.current) return;
        added.push(await importChatImage(file));
      }
      const next = [...images, ...added];
      validateImageSelection(next);
      if (mounted.current) onChange(next);
    } catch (failure) {
      if (mounted.current)
        setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      busy.current = false;
      if (mounted.current) setImporting(false);
    }
  }
  return { addFiles, importing, error };
}
