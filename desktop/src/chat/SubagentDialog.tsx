import { useEffect, useId, useMemo, useRef, useState } from "react";
import {
  orderedSubagents,
  type SubagentCatalogEntry,
} from "./subagentCatalog";
import { subagentStatusLabel, subagentTabLabel } from "./SubagentTabs";

interface SubagentDialogProps {
  readonly entries: readonly SubagentCatalogEntry[];
  /** Opens or activates one child's tab. */
  readonly onOpenChild: (childId: string) => void;
}

/**
 * Composer subagents control: a compact icon button beside the goal control,
 * enabled exactly when this Chat owns at least one child. The dialog lists the
 * Chat's children running-first with status and task; choosing one opens its
 * tab, or activates the tab already open for it.
 */
export function SubagentDialog({
  entries,
  onOpenChild,
}: SubagentDialogProps): React.JSX.Element {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const first = useRef<HTMLButtonElement>(null);
  const popupId = useId();
  const ordered = useMemo(() => orderedSubagents(entries), [entries]);
  const running = ordered.filter((entry) => entry.status === "running").length;

  useEffect(() => {
    if (!open) return;
    first.current?.focus();
    const dismiss = (event: PointerEvent) => {
      if (event.target instanceof Node && !root.current?.contains(event.target)) {
        setOpen(false);
      }
    };
    document.addEventListener("pointerdown", dismiss);
    return () => document.removeEventListener("pointerdown", dismiss);
  }, [open]);

  const close = (restoreFocus: boolean) => {
    setOpen(false);
    if (restoreFocus) trigger.current?.focus();
  };

  const label =
    ordered.length === 0
      ? "No subagents in this Chat"
      : `${ordered.length} subagent(s) in this Chat, ${running} running`;

  return (
    <div
      ref={root}
      className="subagent-control"
      onKeyDown={(event) => {
        if (event.key === "Escape" && open) {
          event.stopPropagation();
          close(true);
        }
      }}
    >
      <button
        ref={trigger}
        className="subagent-button"
        type="button"
        aria-label={label}
        title={`${label} — open a subagent's conversation tab`}
        aria-expanded={open}
        aria-controls={popupId}
        aria-haspopup="dialog"
        disabled={ordered.length === 0}
        onClick={() => setOpen(!open)}
      >
        <svg width="22" height="22" viewBox="0 0 24 24" aria-hidden="true">
          {/* A delegating agent above two delegated children. */}
          <g
            fill="none"
            stroke="currentColor"
            strokeWidth="1.6"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <circle cx="12" cy="5" r="2.4" />
            <circle cx="5.5" cy="18.5" r="2.2" />
            <circle cx="18.5" cy="18.5" r="2.2" />
            <path d="M12 7.4v3.2M12 10.6 5.5 16.3M12 10.6l6.5 5.7" />
          </g>
        </svg>
        {running > 0 && (
          <span className="subagent-button-count" aria-hidden="true">
            {running}
          </span>
        )}
      </button>
      {open && (
        <section
          id={popupId}
          role="dialog"
          aria-label="Subagents in this Chat"
          className="subagent-popover"
        >
          <header className="subagent-popover-heading">
            <strong>Subagents</strong>
            <span>
              {running} running · {ordered.length} total
            </span>
          </header>
          <ul className="subagent-list">
            {ordered.map((entry, index) => (
              <li key={entry.childId}>
                <button
                  ref={index === 0 ? first : undefined}
                  type="button"
                  title={`Open the tab for subagent ${entry.childId}`}
                  onClick={() => {
                    onOpenChild(entry.childId);
                    close(false);
                  }}
                >
                  <span
                    className={`subagent-status-dot ${entry.status}`}
                    aria-hidden="true"
                  />
                  <span className="subagent-list-main">
                    <strong>{subagentTabLabel(entry)}</strong>
                    <small>
                      {entry.kind === "fork" ? "Forked" : "Fresh"} ·{" "}
                      {entry.modelTurns} turn(s) · {entry.toolCalls} tool call(s)
                    </small>
                  </span>
                  <span className={`subagent-status ${entry.status}`}>
                    {subagentStatusLabel(entry.status)}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}
    </div>
  );
}
