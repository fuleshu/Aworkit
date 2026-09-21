import { useEffect, useId, useMemo, useRef, useState } from "react";
import type { RuntimeEvent } from "./corePort";
import { currentGoal, isLiveGoal } from "./goalProjection";
import "./goal.css";

interface Props {
  readonly events: readonly RuntimeEvent[];
  /** Why the goal cannot be changed right now, or null when it can. */
  readonly disabledReason: string | null;
  /** Sets the goal text, or clears it when null. Resolves true when committed. */
  readonly onSubmit: (goal: string | null) => Promise<boolean>;
}

/**
 * Composer goal control. The flag button opens the Chat's durable goal in an
 * editable dialog: the goal is Chat-owned, non-authoritative state, so the user
 * may set, revise or clear it at any time, including while a Run is active.
 * Submitting records the same snapshot the agent's goal tool writes.
 */
export function GoalControl({ events, disabledReason, onSubmit }: Props): React.JSX.Element {
  const goal = useMemo(() => currentGoal(events), [events]);
  const [open, setOpen] = useState(false);
  const [draft, setDraft] = useState(goal?.objective ?? "");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const field = useRef<HTMLTextAreaElement>(null);
  const popupId = useId();

  useEffect(() => {
    if (!open) return;
    field.current?.focus();
    const dismiss = (event: PointerEvent) => {
      if (event.target instanceof Node && !root.current?.contains(event.target)) setOpen(false);
    };
    document.addEventListener("pointerdown", dismiss);
    return () => document.removeEventListener("pointerdown", dismiss);
  }, [open]);

  const live = isLiveGoal(goal);
  const statusLabel =
    goal === null
      ? "No goal"
      : goal.status === "active"
        ? "Active"
        : goal.status === "completed"
          ? "Completed"
          : "Cleared";
  const label =
    goal?.objective === undefined
      ? "Chat goal: not set"
      : `Chat goal (${statusLabel.toLowerCase()}): ${goal.objective}`;

  const close = () => {
    setOpen(false);
    setError(null);
    setDraft(goal?.objective ?? "");
  };

  const submit = async (value: string | null) => {
    setPending(true);
    setError(null);
    try {
      if (await onSubmit(value)) {
        setOpen(false);
        setDraft(value ?? "");
      } else {
        setError("The goal change was not committed.");
      }
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setPending(false);
    }
  };

  return (
    <div
      ref={root}
      className="goal-control"
      onKeyDown={(event) => {
        if (event.key === "Escape" && open) {
          event.stopPropagation();
          close();
        }
      }}
    >
      <button
        ref={trigger}
        className="goal-flag-button"
        type="button"
        aria-label={label}
        title={`${label} — click to view or edit the Chat goal`}
        aria-expanded={open}
        aria-controls={popupId}
        aria-haspopup="dialog"
        onClick={() => {
          setDraft(goal?.objective ?? "");
          setError(null);
          setOpen(!open);
        }}
      >
        <svg width="23" height="23" viewBox="0 0 24 24" aria-hidden="true">
          {/* Two crossed racing flags: splayed poles crossing low, one banner per pole. */}
          <g
            fill={live ? "currentColor" : "none"}
            stroke="currentColor"
            strokeWidth="1.6"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <path d="M6 21 17 4" />
            <path d="M18 21 7 4" />
            <path d="M17 4 22.8 5.4 22 8.8 15 7.4Z" />
            <path d="M7 4 1.2 5.4 2 8.8 9 7.4Z" />
          </g>
        </svg>
      </button>
      {open && (
        <section id={popupId} role="dialog" aria-label="Chat goal" className="goal-popover">
          <header className="goal-popover-heading">
            <strong>Chat goal</strong>
            <span className={`goal-status goal-status-${goal?.status ?? "none"}`}>{statusLabel}</span>
          </header>
          {goal?.note !== undefined && <p className="goal-note">Latest note: {goal.note}</p>}
          <label className="goal-field">
            Goal
            <textarea
              ref={field}
              value={draft}
              rows={4}
              maxLength={8192}
              disabled={pending || disabledReason !== null}
              placeholder="What outcome should this Chat work toward?"
              title="The durable objective this Chat works toward; the agent reads it on every turn"
              onChange={(event) => setDraft(event.target.value)}
            />
          </label>
          <div className="goal-actions">
            <button
              type="button"
              className="primary-action"
              disabled={pending || disabledReason !== null || draft.trim().length === 0}
              title={disabledReason ?? "Save this goal for the Chat"}
              onClick={() => void submit(draft.trim())}
            >
              Submit
            </button>
            <button type="button" disabled={pending} title="Close without changing the goal" onClick={close}>
              Cancel
            </button>
            {live && (
              <button
                type="button"
                className="goal-clear"
                disabled={pending || disabledReason !== null}
                title={disabledReason ?? "Abandon this goal without marking it complete"}
                onClick={() => void submit(null)}
              >
                Clear goal
              </button>
            )}
          </div>
          {disabledReason !== null && <p className="goal-hint">{disabledReason}</p>}
          {error !== null && (
            <p className="goal-error" role="alert">
              {error}
            </p>
          )}
        </section>
      )}
    </div>
  );
}
