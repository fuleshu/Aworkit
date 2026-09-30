import { isTauri } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useEffect, useRef, useState, type MouseEvent } from "react";
import { useNotificationPublisher } from "../notifications/NotificationContext";

/** The product's public home page, opened in the operating system browser. */
export const ABOUT_WEBPAGE_URL = "https://www.klutzgames.com";

/** The public source repository, opened in the operating system browser. */
export const ABOUT_SOURCE_URL = "https://github.com/fuleshu/Aworkit";

/** A third-party library the build depends on and the license it grants. */
export interface AboutLibrary {
  readonly name: string;
  readonly license: string;
}

/**
 * The major libraries Aworkit builds on. Curated, not discovered at runtime:
 * the dialog is a credit and license notice, so it must not add work to startup
 * and must stay readable even when the native projection is unavailable.
 */
export const ABOUT_LIBRARIES: readonly AboutLibrary[] = [
  { name: "Tauri", license: "Apache-2.0 OR MIT" },
  { name: "React", license: "MIT" },
  { name: "Mantine", license: "MIT" },
  { name: "React Flow", license: "MIT" },
  { name: "Vite", license: "MIT" },
  { name: "TypeScript", license: "Apache-2.0" },
  { name: "Zod", license: "MIT" },
  { name: "Rig.rs", license: "MIT" },
  { name: "Tokio", license: "MIT" },
  { name: "Reqwest", license: "MIT OR Apache-2.0" },
  { name: "rmcp (MCP SDK)", license: "Apache-2.0" },
  { name: "rusqlite / SQLite", license: "MIT / Public Domain" },
  { name: "Serde", license: "MIT OR Apache-2.0" },
  { name: "image", license: "MIT OR Apache-2.0" },
  { name: "tree-sitter", license: "MIT" },
  { name: "cap-std", license: "Apache-2.0 WITH LLVM-exception" },
  { name: "scraper", license: "ISC" },
  { name: "keyring", license: "MIT OR Apache-2.0" },
];

/**
 * The modal About surface behind Help → About.
 *
 * It is an in-workbench dialog rather than a native OS message box because the
 * specification requires activatable links; a native message dialog cannot open
 * the home page or repository. The native `<dialog>` owns the backdrop, the
 * focus trap and Escape, and focus returns to whatever had it when the dialog
 * closes. Content is authored here, never fetched.
 */
export function AboutDialog({
  onClose,
}: {
  readonly onClose: () => void;
}): React.JSX.Element {
  const dialog = useRef<HTMLDialogElement>(null);
  const [version, setVersion] = useState<string | null>(null);
  const notifications = useNotificationPublisher("About", "web-links", "shell");
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
  // The version is native metadata; a browser preview and a denied IPC simply
  // leave the line out instead of fabricating one.
  useEffect(() => {
    let current = true;
    void getVersion()
      .then((value) => {
        if (current) setVersion(value);
      })
      .catch(() => undefined);
    return () => {
      current = false;
    };
  }, []);
  const activate = (event: MouseEvent<HTMLAnchorElement>, url: string) => {
    // A browser-hosted preview keeps standard new-tab behavior.
    if (!isTauri()) return;
    event.preventDefault();
    void openUrl(url).catch(() => {
      notifications.publish("open-link", {
        summary: "Could not open the link in your default browser.",
        detail: url,
        severity: "error",
        lifetime: { kind: "transient" },
      });
    });
  };
  return (
    <dialog
      ref={dialog}
      aria-labelledby="about-dialog-title"
      className="workbench-dialog about-dialog"
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
      onKeyDown={(event) => {
        // Keep Escape from also reaching the Settings leave guard behind it.
        if (event.key === "Escape") event.stopPropagation();
      }}
    >
      <h2 id="about-dialog-title">About Aworkit</h2>
      {version !== null && <p className="about-version">Version {version}</p>}
      <dl className="about-meta">
        <dt>Developer</dt>
        <dd>Timo Fleisch</dd>
        <dt>Company</dt>
        <dd>d.b.a. klutzGames</dd>
        <dt>Webpage</dt>
        <dd>
          <a
            href={ABOUT_WEBPAGE_URL}
            rel="noreferrer"
            target="_blank"
            onClick={(event) => activate(event, ABOUT_WEBPAGE_URL)}
          >
            {ABOUT_WEBPAGE_URL}
          </a>
        </dd>
        <dt>GitHub</dt>
        <dd>
          <a
            href={ABOUT_SOURCE_URL}
            rel="noreferrer"
            target="_blank"
            onClick={(event) => activate(event, ABOUT_SOURCE_URL)}
          >
            {ABOUT_SOURCE_URL}
          </a>
        </dd>
        <dt>License</dt>
        <dd>Apache-2.0</dd>
      </dl>
      <h3>Third-party libraries</h3>
      <ul className="about-libraries">
        {ABOUT_LIBRARIES.map((library) => (
          <li key={library.name}>
            <span>{library.name}</span>
            <span className="about-license">{library.license}</span>
          </li>
        ))}
      </ul>
      <div className="about-actions">
        <button
          className="primary-action"
          title="Close the About dialog"
          type="button"
          onClick={onClose}
        >
          Close
        </button>
      </div>
    </dialog>
  );
}
