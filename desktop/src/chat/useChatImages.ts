import { useEffect, useRef, useState } from "react";
import {
  importChatImage,
  validateImageSelection,
  type ImageAttachment,
} from "./images";

/** Serializes imports, retains existing attachments on failure, and prevents a
 * slow file read from adding images to a different Chat after navigation. Any
 * number of images may be added; there is no count or total-size limit. */
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
