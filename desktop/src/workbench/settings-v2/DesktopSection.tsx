import type { DesktopConfiguration } from "../configuration";

/**
 * How the desktop opens a path a conversation showed. The editor command is
 * part of the Settings document, so it follows the ordinary Save/Discard cycle
 * and is validated as a launchable command before it is stored. An empty field
 * means no editor is configured, and open-in-editor falls back to the
 * operating system's own default application for the file's format.
 */
export function DesktopSection({
  value,
  onChange,
}: {
  readonly value: DesktopConfiguration;
  readonly onChange: (value: DesktopConfiguration) => void;
}): React.JSX.Element {
  return (
    <div className="settings-section-stack">
      <h3 id="settings-desktop-heading">Opening files from a conversation</h3>
      <p className="section-intro">
        Right-click a file path in a conversation to open it, open it in your
        editor, or reveal it in your file manager. A path is only ever acted on
        inside the Chat's own workspace folder.
      </p>
      <label className="settings-field" htmlFor="desktop-editor">
        Editor command
        <input
          id="desktop-editor"
          placeholder="code"
          title="One command name resolved from PATH, or an absolute path to an executable. Leave empty to use the operating system's default application."
          type="text"
          value={value.editor ?? ""}
          onChange={(event) => {
            const editor = event.target.value;
            // Empty means "no editor configured"; the document then omits the
            // key instead of storing a command the host would refuse.
            onChange(editor.trim() === "" ? {} : { editor });
          }}
        />
      </label>
      <p className="settings-field-help">
        {value.editor === undefined || value.editor.trim() === ""
          ? "No editor configured. Open in editor uses your operating system's default application."
          : `Open in editor starts "${value.editor.trim()}" with the selected file.`}
      </p>
    </div>
  );
}
