import { useCallback, useEffect, useState } from "react";
import {
  documentsExtrasStatus,
  openDocumentsFolder,
  setDocumentsExtrasEnabled,
  writeDocumentsExtras,
  type DocumentsExtrasReport,
} from "../../shell/documentsExtras";

/**
 * The opt-out for the bundled example workflows and FFmpeg plugin.
 *
 * The extras are application resources; this control decides whether the native
 * host also copies them into the user's documents folder. It acts immediately
 * rather than through the Settings draft, because it changes files on disk and
 * not the Settings document. In a browser preview it renders inert, because
 * there is no native host to write anything.
 */
export function DocumentsExtrasControl(): React.JSX.Element {
  const [report, setReport] = useState<DocumentsExtrasReport | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let current = true;
    void documentsExtrasStatus()
      .then((value) => {
        if (current) setReport(value);
      })
      .catch((cause: unknown) => {
        if (current) setError(String(cause));
      });
    return () => {
      current = false;
    };
  }, []);

  const run = useCallback(
    async (action: () => Promise<DocumentsExtrasReport | null>) => {
      setBusy(true);
      setError(null);
      try {
        setReport(await action());
      } catch (cause: unknown) {
        setError(String(cause));
      } finally {
        setBusy(false);
      }
    },
    [],
  );

  const enabled = report?.enabled ?? false;
  const unavailable = report === null;

  return (
    <div className="settings-subsection">
      <h4>Example workflows and FFmpeg plugin</h4>
      <p className="settings-field-help">
        Every installation bundles four example workflows and the reference
        FFmpeg plugin. They are copied into an Aworkit folder inside your
        documents folder, where you can open and edit them.
      </p>
      <label className="settings-checkbox" htmlFor="documents-extras-enabled">
        <input
          id="documents-extras-enabled"
          checked={enabled}
          disabled={busy || unavailable}
          title="Copy the example workflows and the FFmpeg plugin into your documents folder"
          type="checkbox"
          onChange={(event) => {
            const next = event.target.checked;
            void run(() => setDocumentsExtrasEnabled(next));
          }}
        />
        Write them into my documents folder
      </label>
      <p className="settings-field-help" role="status">
        {unavailable
          ? "Available in the desktop app."
          : report.folder === null
            ? "This system has no documents folder, so the extras stay bundled with the app."
            : `${report.folder}${describeContents(report)}`}
      </p>
      <div className="section-actions">
        <button
          disabled={busy || unavailable}
          title="Write any missing example files and the plugin, keeping the files you changed"
          type="button"
          onClick={() => void run(writeDocumentsExtras)}
        >
          Write now
        </button>
        <button
          disabled={busy || unavailable}
          title="Reveal the Aworkit folder in your file manager"
          type="button"
          onClick={() =>
            void run(async () => {
              await openDocumentsFolder();
              return documentsExtrasStatus();
            })
          }
        >
          Open folder
        </button>
      </div>
      {error !== null && (
        <p className="field-error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

/** One short sentence describing what the folder currently holds. */
function describeContents(report: DocumentsExtrasReport): string {
  if (!report.exists) return " — not written yet";
  const workflows = `${report.exampleWorkflowCount} example workflow${
    report.exampleWorkflowCount === 1 ? "" : "s"
  }`;
  return ` — ${workflows}, FFmpeg plugin ${report.pluginPresent ? "present" : "missing"}`;
}
