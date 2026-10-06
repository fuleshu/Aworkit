import { isTauri } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useCallback, useEffect, useRef, useState } from "react";
import { useNotificationPublisher } from "../notifications/NotificationContext";
import { openDocumentsFolder } from "./documentsExtras";
import { DocumentationMarkdown } from "./DocumentationMarkdown";
import {
  userHelpDocument,
  userHelpHomeDocument,
  type UserHelpTarget,
} from "./userHelp";

/** The back/forward stack: the visited documents and the current position. */
interface DocumentationHistory {
  readonly entries: readonly string[];
  readonly index: number;
}

/** The home document; the build ships it, so it always resolves. */
const HOME_DOCUMENT = userHelpHomeDocument();

/** Finds a heading by its exact id without needing a CSS escaper. */
function findAnchor(container: HTMLElement, anchor: string): HTMLElement | null {
  for (const element of container.querySelectorAll<HTMLElement>("[id]")) {
    if (element.id === anchor) return element;
  }
  return null;
}

/** Scrolls the reading area to a heading, when that heading exists. */
function scrollToAnchor(container: HTMLElement | null, anchor: string): void {
  if (container === null) return;
  findAnchor(container, anchor)?.scrollIntoView({ block: "start" });
}

function BackIcon(): React.JSX.Element {
  return (
    <svg aria-hidden="true" height="16" viewBox="0 0 16 16" width="16">
      <path
        d="M10 3 5 8l5 5"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="1.6"
      />
    </svg>
  );
}

function ForwardIcon(): React.JSX.Element {
  return (
    <svg aria-hidden="true" height="16" viewBox="0 0 16 16" width="16">
      <path
        d="m6 3 5 5-5 5"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="1.6"
      />
    </svg>
  );
}

function HomeIcon(): React.JSX.Element {
  return (
    <svg aria-hidden="true" height="16" viewBox="0 0 16 16" width="16">
      <path
        d="M2.8 7.6 8 3.4l5.2 4.2V13H2.8zM6.4 13V9.4h3.2V13"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="1.4"
      />
    </svg>
  );
}

/**
 * The modal Documentation panel behind Help → Documentation.
 *
 * It renders the Markdown documents bundled from `desktop/user_help`. A
 * document may link to another document or to a heading, so the panel keeps its
 * own back/forward/home history; external `http(s)` links open in the system
 * browser instead of navigating the app WebView. The native `<dialog>` owns the
 * backdrop, focus trap and Escape, and focus returns to whatever had it.
 */
export function DocumentationDialog({
  onClose,
}: {
  readonly onClose: () => void;
}): React.JSX.Element {
  const dialog = useRef<HTMLDialogElement>(null);
  const body = useRef<HTMLDivElement>(null);
  const [history, setHistory] = useState<DocumentationHistory>({
    entries: [HOME_DOCUMENT.id],
    index: 0,
  });
  const [pendingAnchor, setPendingAnchor] = useState<string | null>(null);
  const notifications = useNotificationPublisher(
    "Documentation",
    "web-links",
    "shell",
  );
  const helpDocument =
    userHelpDocument(history.entries[history.index]) ?? HOME_DOCUMENT;
  const canGoBack = history.index > 0;
  const canGoForward = history.index < history.entries.length - 1;

  useEffect(() => {
    const previous =
      document.activeElement instanceof HTMLElement ? document.activeElement : null;
    // StrictMode re-runs this effect before its cleanup, and a modal dialog
    // refuses a second showModal while it is already open.
    if (dialog.current !== null && !dialog.current.open) {
      dialog.current.showModal();
    }
    return () => {
      if (previous?.isConnected) previous.focus({ preventScroll: true });
    };
  }, []);

  // A freshly opened document starts at the top of the reading area.
  useEffect(() => {
    if (body.current !== null) body.current.scrollTop = 0;
  }, [helpDocument.id]);

  // An in-page anchor keeps the current document and only scrolls.
  useEffect(() => {
    if (pendingAnchor === null) return;
    scrollToAnchor(body.current, pendingAnchor);
    setPendingAnchor(null);
  }, [pendingAnchor, helpDocument.id]);

  const goTo = useCallback((id: string, anchor: string | null) => {
    setHistory((current) => {
      if (current.entries[current.index] === id) return current;
      const entries = [...current.entries.slice(0, current.index + 1), id];
      return { entries, index: entries.length - 1 };
    });
    setPendingAnchor(anchor);
  }, []);

  const goBack = useCallback(() => {
    setHistory((current) =>
      current.index > 0 ? { ...current, index: current.index - 1 } : current,
    );
    setPendingAnchor(null);
  }, []);

  const goForward = useCallback(() => {
    setHistory((current) =>
      current.index < current.entries.length - 1
        ? { ...current, index: current.index + 1 }
        : current,
    );
    setPendingAnchor(null);
  }, []);

  const goHome = useCallback(() => goTo(HOME_DOCUMENT.id, null), [goTo]);

  const follow = useCallback(
    (target: UserHelpTarget) => {
      switch (target.kind) {
        case "document":
          goTo(target.documentId, target.anchor);
          return;
        case "anchor":
          scrollToAnchor(body.current, target.anchor);
          return;
        case "external":
          if (!isTauri()) {
            window.open(target.url, "_blank", "noreferrer");
            return;
          }
          void openUrl(target.url).catch(() => {
            notifications.publish("open-link", {
              summary: "Could not open the link in your default browser.",
              detail: target.url,
              severity: "error",
              lifetime: { kind: "transient" },
            });
          });
          return;
        case "documents-folder":
          // The native host owns the path; the panel only asks it to open.
          void openDocumentsFolder().catch(() => {
            notifications.publish("open-folder", {
              summary: "Could not open the Aworkit documents folder.",
              severity: "error",
              lifetime: { kind: "transient" },
            });
          });
          return;
        case "unsupported":
          return;
      }
    },
    [goTo, notifications],
  );

  return (
    <dialog
      ref={dialog}
      aria-labelledby="documentation-title"
      className="workbench-dialog documentation-dialog"
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
      onKeyDown={(event) => {
        // Keep Escape from also reaching the Settings leave guard behind it.
        if (event.key === "Escape") event.stopPropagation();
      }}
    >
      <header className="documentation-toolbar">
        <div className="documentation-nav">
          <button
            aria-label="Back"
            disabled={!canGoBack}
            title="Go back to the previous document"
            type="button"
            onClick={goBack}
          >
            <BackIcon />
          </button>
          <button
            aria-label="Forward"
            disabled={!canGoForward}
            title="Go forward to the next document"
            type="button"
            onClick={goForward}
          >
            <ForwardIcon />
          </button>
          <button
            aria-label="Home"
            title="Go to the documentation home page"
            type="button"
            onClick={goHome}
          >
            <HomeIcon />
          </button>
        </div>
        <h2 className="documentation-title" id="documentation-title">
          {helpDocument.title}
        </h2>
        <button
          className="documentation-close"
          title="Close the documentation"
          type="button"
          onClick={onClose}
        >
          Close
        </button>
      </header>
      <div className="documentation-body" ref={body} tabIndex={0}>
        <DocumentationMarkdown
          markdown={helpDocument.markdown}
          onFollow={follow}
        />
      </div>
    </dialog>
  );
}
