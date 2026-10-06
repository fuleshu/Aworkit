/**
 * The Markdown help documents the Documentation panel renders.
 *
 * Every document is one `.md` file in `desktop/user_help`. They are bundled at
 * build time, so the panel needs no filesystem access, no IPC and no network:
 * an installed app renders exactly the documents it shipped with. The file
 * name without its extension is the document id that links and history use.
 */

const rawDocuments = import.meta.glob("../../user_help/*.md", {
  eager: true,
  query: "?raw",
  import: "default",
}) as Record<string, string>;

/** One bundled help document, addressed by its file name without `.md`. */
export interface UserHelpDocument {
  readonly id: string;
  /** The first level-one heading, or the id when a document has none. */
  readonly title: string;
  readonly markdown: string;
}

/** The document the panel opens on and the Home button returns to. */
export const USER_HELP_HOME_ID = "index";

/** The file name (without extension) behind one bundled module path. */
function documentIdOf(modulePath: string): string {
  const file = modulePath.slice(modulePath.lastIndexOf("/") + 1);
  return file.replace(/\.md$/iu, "");
}

/** The document title: its first `# ` heading, with inline markup removed. */
function titleOf(markdown: string, fallback: string): string {
  const heading = /^#\s+(.+?)\s*$/mu.exec(markdown);
  if (heading === null) return fallback;
  return heading[1].replaceAll(/[`*_]/gu, "").trim() || fallback;
}

const documents = new Map<string, UserHelpDocument>();
for (const [modulePath, markdown] of Object.entries(rawDocuments)) {
  const id = documentIdOf(modulePath);
  documents.set(id, { id, title: titleOf(markdown, id), markdown });
}

/** Every bundled document id, sorted, for tests and diagnostics. */
export const USER_HELP_DOCUMENT_IDS: readonly string[] = [...documents.keys()].sort();

/** The document for an id, or null when the id is not bundled. */
export function userHelpDocument(id: string): UserHelpDocument | null {
  return documents.get(id) ?? null;
}

/**
 * The home document. The build always ships `index.md`; a missing one is a
 * packaging error, so it fails loudly rather than rendering an empty panel.
 */
export function userHelpHomeDocument(): UserHelpDocument {
  const home = documents.get(USER_HELP_HOME_ID);
  if (home === undefined) {
    throw new Error(`the bundled help is missing ${USER_HELP_HOME_ID}.md`);
  }
  return home;
}

/**
 * The help-document link that reveals the Aworkit folder in the documents
 * directory. A dedicated scheme keeps a local-file action out of ordinary
 * prose; the panel resolves it and never lets a document open an arbitrary path.
 */
export const DOCUMENTS_FOLDER_HREF = "aworkit:documents";

/**
 * What a clicked Markdown link means inside the documentation panel.
 *
 * - `document` — another bundled help document, optionally at an anchor.
 * - `anchor` — a heading in the current document.
 * - `external` — an absolute HTTP(S) address for the system browser.
 * - `documents-folder` — the Aworkit folder in the user's documents directory.
 * - `unsupported` — anything else; the link is inert on purpose.
 */
export type UserHelpTarget =
  | {
      readonly kind: "document";
      readonly documentId: string;
      readonly anchor: string | null;
    }
  | { readonly kind: "anchor"; readonly anchor: string }
  | { readonly kind: "external"; readonly url: string }
  | { readonly kind: "documents-folder" }
  | { readonly kind: "unsupported" };

/** Splits `path#anchor` into its two parts; the anchor may be absent. */
function splitFragment(value: string): readonly [string, string | null] {
  const hash = value.indexOf("#");
  if (hash < 0) return [value, null];
  const anchor = value.slice(hash + 1);
  return [value.slice(0, hash), anchor === "" ? null : anchor];
}

/**
 * Resolves one Markdown `href` from a help document.
 *
 * Only same-folder `.md` documents that are actually bundled navigate inside
 * the panel, so a typo cannot leave the reader on a blank page. Anything with a
 * scheme other than `http`/`https` is inert, exactly as chat citations are.
 */
export function resolveUserHelpTarget(href: string | undefined): UserHelpTarget {
  if (href === undefined) return { kind: "unsupported" };
  const value = href.trim();
  if (value === "") return { kind: "unsupported" };
  if (value.startsWith("#")) {
    const anchor = value.slice(1);
    return anchor === "" ? { kind: "unsupported" } : { kind: "anchor", anchor };
  }
  if (value.toLowerCase() === DOCUMENTS_FOLDER_HREF) {
    return { kind: "documents-folder" };
  }
  if (/^https?:\/\//iu.test(value)) return { kind: "external", url: value };
  if (/^[a-z][a-z0-9+.-]*:/iu.test(value)) return { kind: "unsupported" };
  const [path, anchor] = splitFragment(value);
  if (!/\.md$/iu.test(path)) return { kind: "unsupported" };
  const id = documentIdOf(path.replace(/^\.\//u, ""));
  if (!documents.has(id)) return { kind: "unsupported" };
  return { kind: "document", documentId: id, anchor };
}

/**
 * The GitHub-style id a heading gets, so a `#anchor` link can find it.
 *
 * The same shape as the anchors readers copy from GitHub: lowercase, inline
 * markup removed, runs of spaces and hyphens collapsed to one hyphen.
 */
export function slugifyHeading(text: string): string {
  return text
    .toLowerCase()
    .replaceAll(/[^\p{L}\p{N}\s-]/gu, "")
    .trim()
    .replaceAll(/[\s-]+/gu, "-")
    .replaceAll(/^-+|-+$/gu, "");
}
