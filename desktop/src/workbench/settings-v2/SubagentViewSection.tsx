import { useEffect, useState } from "react";
import {
  createSubagentViewPreferencePort,
  DEFAULT_SUBAGENT_VIEW,
  type SubagentViewPreference,
  type SubagentViewPreferencePort,
  type SubagentViewPreferenceSnapshot,
} from "../../chat/subagentViewPreference";

/**
 * Delegated-subagent tab presentation. It is a global user preference with a
 * version, committed through its own dedicated command — it is never part of a
 * Chat's frozen tool contract, so it does not participate in the Settings
 * draft's Save/Discard cycle. A conflict, rejection or uncertain update returns
 * to the latest projected values while unrelated drafts are untouched.
 */
export function SubagentViewSection({
  port,
  onChange,
}: {
  readonly port?: SubagentViewPreferencePort;
  readonly onChange?: (preference: SubagentViewPreference) => void;
}): React.JSX.Element {
  const [resolved] = useState<SubagentViewPreferencePort>(
    () => port ?? createSubagentViewPreferencePort(),
  );
  const [value, setValue] =
    useState<SubagentViewPreferenceSnapshot>(DEFAULT_SUBAGENT_VIEW);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);

  useEffect(() => {
    let current = true;
    void resolved
      .snapshot()
      .then((stored) => {
        if (!current) return;
        setValue(stored);
        onChange?.(stored);
      })
      .catch((failure: unknown) => {
        if (current) {
          setError(
            failure instanceof Error
              ? `Could not load the subagent view preference: ${failure.message}`
              : "Could not load the subagent view preference.",
          );
        }
      });
    return () => {
      current = false;
    };
    // The preference is read once when the section mounts; every later change
    // is the value this section itself committed.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [resolved]);

  const commit = async (next: SubagentViewPreference) => {
    const previous = value;
    setValue({ ...next, version: previous.version });
    setPending(true);
    setError(null);
    setStatus("Saving…");
    try {
      await resolved.commit(next, previous.version);
      // A successful write advances the document version, so the section reads
      // the projected values again instead of assuming its own.
      const latest = await resolved.snapshot();
      setValue(latest);
      onChange?.(latest);
      setStatus("Saved");
    } catch (failure) {
      setValue(previous);
      setStatus(null);
      setError(
        failure instanceof Error
          ? failure.message
          : "The subagent view preference was not saved.",
      );
    } finally {
      setPending(false);
    }
  };

  return (
    <div className="settings-section-stack" aria-labelledby="settings-subagents-heading">
      <h3 id="settings-subagents-heading">Subagent tabs</h3>
      <p className="section-intro">
        How this desktop opens and closes the conversation tab of a delegated
        subagent. These are presentation preferences and never change what a
        subagent may do.
      </p>
      <label className="settings-field settings-checkbox-field">
        <input
          checked={value.autoOpen}
          disabled={pending}
          title="Open a newly created subagent's tab in the background without changing the active tab"
          type="checkbox"
          onChange={(event) =>
            void commit({
              autoOpen: event.target.checked,
              autoClose: value.autoClose,
            })
          }
        />
        <span>
          <strong>Open new subagent tabs in the background</strong>
          <small>
            A newly created child gets a tab without moving focus away from the
            tab you are reading.
          </small>
        </span>
      </label>
      <label className="settings-field settings-checkbox-field">
        <input
          checked={value.autoClose}
          disabled={pending}
          title="Close a subagent's tab when it settles, unless that tab is the active one"
          type="checkbox"
          onChange={(event) =>
            void commit({
              autoOpen: value.autoOpen,
              autoClose: event.target.checked,
            })
          }
        />
        <span>
          <strong>Close settled subagent tabs</strong>
          <small>
            When a child finishes, fails, is interrupted or is cancelled, its
            tab closes unless it is the active tab. A tab you closed yourself
            stays closed.
          </small>
        </span>
      </label>
      {error !== null ? (
        <p className="settings-field-help settings-field-error" role="alert">
          {error}
        </p>
      ) : (
        <p className="settings-field-help" role="status" aria-live="polite">
          {status ?? "Preference is stored for this desktop."}
        </p>
      )}
    </div>
  );
}
