import { useEffect, useRef } from "react";

/** Only an actual unsaved-change decision interrupts navigation; notifications never do. */
export function SettingsLeaveDialog({ busy, canSave, onSave, onDiscard, onStay }: {
  readonly busy: boolean; readonly canSave: boolean;
  readonly onSave: () => void; readonly onDiscard: () => void; readonly onStay: () => void;
}): React.JSX.Element {
  const dialog = useRef<HTMLElement>(null);
  const stay = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    stay.current?.focus();
    return () => { if (previous?.isConnected && !previous.closest("[hidden]")) previous.focus({ preventScroll: true }); };
  }, []);
  return <div className="dialog-backdrop"><section ref={dialog} role="dialog" aria-modal="true" aria-labelledby="settings-leave-title" className="workbench-dialog"
    onKeyDown={event => {
      if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); if (!busy) onStay(); }
      if (event.key !== "Tab") return;
      const controls = Array.from(dialog.current?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)") ?? []);
      const first = controls[0], last = controls.at(-1);
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
    }}>
    <h2 id="settings-leave-title">Save changes before returning?</h2>
    <p>Your Settings changes have not been saved.</p>
    <div className="settings-leave-actions">
      <button type="button" className="primary-action" disabled={busy || !canSave} title={canSave ? "Save Settings and return to your workspace" : "Resolve invalid fields or finish storing credential input before saving"} onClick={onSave}>Save and return</button>
      <button type="button" disabled={busy} title="Discard Settings edits and return to your workspace" onClick={onDiscard}>Discard and return</button>
      <button type="button" ref={stay} disabled={busy} title="Keep your draft and continue editing Settings" onClick={onStay}>Stay in Settings</button>
    </div>
  </section></div>;
}
