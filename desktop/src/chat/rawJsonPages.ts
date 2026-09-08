/** Bound wrapped-text layout even when a single JSON string spans megabytes. */
export const RAW_JSON_PAGE_CHARACTERS = 32_768;

export function rawJsonPages(raw: string): readonly string[] {
  const pages: string[] = [];
  for (let start = 0; start < raw.length;) {
    let end = Math.min(start + RAW_JSON_PAGE_CHARACTERS, raw.length);
    if (end < raw.length) {
      // Prefer complete lines, but never let one enormous value defeat the cap.
      const newline = start + raw.slice(start, end).lastIndexOf("\n");
      if (newline > start + RAW_JSON_PAGE_CHARACTERS / 2) end = newline + 1;
      else if (raw.charCodeAt(end - 1) >= 0xd800 && raw.charCodeAt(end - 1) <= 0xdbff) end--;
    }
    pages.push(raw.slice(start, end));
    start = end;
  }
  return pages.length > 0 ? pages : [""];
}
