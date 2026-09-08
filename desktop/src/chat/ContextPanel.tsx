import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { contextDocumentSchema, type ContextDocument, type ContextSelection } from "./contextProjection";

interface Props {
  readonly selection: ContextSelection;
  readonly editDisabledReason: string | null;
  readonly onSave: (selection: ContextSelection, document: ContextDocument) => Promise<boolean>;
  readonly onClose: () => void;
}

/** Modal snapshot: read-only until explicitly unlocked; every close route commits dirty edits. */
export function ContextPanel({ selection, editDisabledReason, onSave, onClose }: Props): React.JSX.Element {
  const dialog = useRef<HTMLDialogElement>(null);
  const editor = useRef<HTMLTextAreaElement>(null);
  const [editing, setEditing] = useState(false);
  const [source, setSource] = useState(() => JSON.stringify(selection.document, null, 2));
  const [wrap, setWrap] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const savingRef = useRef(false);
  const dirty = source !== JSON.stringify(selection.document, null, 2);
  useEffect(() => {
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    dialog.current?.showModal();
    return () => { if (previous?.isConnected) previous.focus(); };
  }, []);
  const close = async () => {
    if (savingRef.current) return;
    if (!dirty) { onClose(); return; }
    if (editDisabledReason) { setError(editDisabledReason); return; }
    let document: ContextDocument;
    try {
      document = contextDocumentSchema.parse(JSON.parse(source));
    } catch (failure) {
      setError(failure instanceof SyntaxError ? failure.message : "Invalid context structure. Keep input.messages, tools, exchanges and contextMessages in their original structure.");
      return;
    }
    savingRef.current = true;
    setSaving(true); setError(null);
    try {
      if (await onSave(selection, document)) onClose();
      else setError("Context could not be saved. Your edits are still here; check the error notification before retrying.");
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally { savingRef.current = false; setSaving(false); }
  };
  return createPortal(<dialog ref={dialog} className="context-panel" aria-labelledby="context-panel-title"
    onCancel={event => { event.preventDefault(); void close(); }}>
    <header className="context-panel-header">
      <div><h2 id="context-panel-title">Model context</h2><p>{selection.label} · {editing ? "Editing enabled" : "Read only"}</p></div>
      <button type="button" aria-label="Close context" title={dirty ? "Save context edits, record an event and close" : "Close context"}
        disabled={saving} onClick={() => void close()}>×</button>
    </header>
    <div className="context-panel-toolbar">
      <p>Raw model messages, tool definitions and exchanges. Images appear as references.</p>
      <label title="Wrap long lines in the context viewer"><input type="checkbox" checked={wrap} onChange={event => setWrap(event.target.checked)} /> Wrap lines</label>
      {!editing && <button type="button" disabled={editDisabledReason !== null}
        title={editDisabledReason ?? "Unlock raw context editing for subsequent calls of this workflow node"}
        onClick={() => { setEditing(true); window.requestAnimationFrame(() => editor.current?.focus()); }}>Enable Edit</button>}
    </div>
    {editing && <p className="context-edit-note">Changes apply when you close this panel. Tool definitions remain frozen for this Chat.</p>}
    {editDisabledReason && <p className="context-edit-note">{editDisabledReason}</p>}
    <textarea ref={editor} className={"context-source" + (wrap ? " context-source-wrap" : "")}
      aria-label="Raw model context" title={editing ? "Edit valid JSON; closing saves changes and records a Context edited event" : "Read-only structured JSON of the current model context"}
      readOnly={!editing || saving} spellCheck={false} value={source}
      onChange={event => { setSource(event.target.value); setError(null); }} />
    {error && <p className="context-panel-error" role="alert">{error}</p>}
    <footer className="context-panel-footer">
      <span>{dirty ? "Unsaved context changes" : "Context snapshot"} · {source.split("\n").length.toLocaleString()} lines</span>
      <div>
        {dirty && <button type="button" disabled={saving} title="Discard your context edits and close without recording a change" onClick={onClose}>Discard changes</button>}
        <button type="button" className={dirty ? "primary-action" : ""} disabled={saving}
          title={dirty ? "Validate and save context, record an event, then close" : "Close context panel"} onClick={() => void close()}>
          {saving ? "Saving…" : dirty ? "Save and Close" : "Close"}
        </button>
      </div>
    </footer>
  </dialog>, document.body);
}
