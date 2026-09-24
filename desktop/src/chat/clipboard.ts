import { useCallback, useEffect, useRef, useState } from "react";

/** How long a control keeps its confirmation after a successful write. */
const COPIED_FEEDBACK_MS = 1000;

/**
 * Writes text to the host clipboard.
 *
 * The async Clipboard API is absent on insecure hosts such as a `file://` page
 * or jsdom, so the deprecated `execCommand` path is tried before giving up, and
 * a refused write resolves false instead of throwing: a control must be able to
 * report the failure rather than claim a copy that never happened.
 */
export async function writeClipboard(text: string): Promise<boolean> {
  if (navigator.clipboard?.writeText !== undefined) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      return false;
    }
  }
  const exec =
    typeof document.execCommand === "function"
      ? document.execCommand.bind(document)
      : undefined;
  if (exec === undefined) return false;
  const field = document.createElement("textarea");
  field.value = text;
  field.setAttribute("readonly", "");
  field.style.position = "fixed";
  field.style.left = "-9999px";
  document.body.appendChild(field);
  field.select();
  try {
    return exec("copy");
  } catch {
    return false;
  } finally {
    field.remove();
  }
}

/** The transient confirmation flag and the handler that sets it. */
export interface CopyFeedback {
  /** True for one second after a successful write. */
  readonly copied: boolean;
  /** Copies the hook's text; a refused write leaves the flag false. */
  readonly copy: () => void;
}

/** Copies `text` and keeps `copied` true briefly so the control can confirm it. */
export function useCopyFeedback(text: string): CopyFeedback {
  const [copied, setCopied] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(
    () => () => {
      if (timer.current !== null) clearTimeout(timer.current);
    },
    [],
  );
  const copy = useCallback(() => {
    if (copied) return;
    void writeClipboard(text).then((accepted) => {
      if (!accepted) return;
      setCopied(true);
      if (timer.current !== null) clearTimeout(timer.current);
      timer.current = setTimeout(() => {
        timer.current = null;
        setCopied(false);
      }, COPIED_FEEDBACK_MS);
    });
  }, [copied, text]);
  return { copied, copy };
}
