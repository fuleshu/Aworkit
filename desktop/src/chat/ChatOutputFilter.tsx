import { useEffect, useId, useRef, useState } from "react";
import type { ChatOutputFilterState } from "./chatOutputFilter";
import "./chatOutputFilter.css";

interface Props {
  readonly value: ChatOutputFilterState;
  readonly onChange: (value: ChatOutputFilterState) => void;
}

/**
 * Header control that chooses which optional Chat output entries are shown.
 *
 * The choice is presentation-only session state: it never changes what the Run
 * records, and flipping a box re-renders the already-rendered transcript.
 */
export function ChatOutputFilter({ value, onChange }: Props): React.JSX.Element {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const popupId = useId();
  const hidden = [
    !value.thinking && "reasoning",
    !value.tools && "tools",
  ].filter((entry): entry is string => entry !== false);
  const label =
    hidden.length === 0
      ? "Chat output filter"
      : `Chat output filter (hiding ${hidden.join(" and ")})`;

  useEffect(() => {
    if (!open) return;
    const dismiss = (event: PointerEvent) => {
      if (
        event.target instanceof Node &&
        !root.current?.contains(event.target)
      ) {
        setOpen(false);
      }
    };
    document.addEventListener("pointerdown", dismiss);
    return () => document.removeEventListener("pointerdown", dismiss);
  }, [open]);

  /** Closes the popover; focus returns to the button unless a pointer did it. */
  const close = (restoreFocus: boolean) => {
    setOpen(false);
    if (restoreFocus) trigger.current?.focus();
  };

  return (
    <div
      ref={root}
      className="output-filter"
      onKeyDown={(event) => {
        if (event.key === "Escape" && open) {
          event.stopPropagation();
          close(true);
        }
      }}
    >
      <button
        ref={trigger}
        className="output-filter-button"
        type="button"
        aria-label={label}
        aria-expanded={open}
        aria-controls={popupId}
        aria-haspopup="dialog"
        title={`${label} — choose which Chat entries are shown`}
        onClick={() => setOpen(!open)}
      >
        <svg width="18" height="18" viewBox="0 0 24 24" aria-hidden="true">
          {/* A funnel: the Chat output filter. */}
          <path
            d="M4 5h16l-6 7v6l-4 2v-8L4 5Z"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.8"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
      </button>
      {open && (
        <section
          id={popupId}
          role="dialog"
          aria-label="Chat output filter"
          className="output-filter-popover"
        >
          <header className="output-filter-heading">
            <strong>Show in Chat</strong>
          </header>
          <label className="output-filter-option">
            <input
              type="checkbox"
              checked={value.thinking}
              onChange={(event) =>
                onChange({ ...value, thinking: event.target.checked })
              }
            />
            <span>Thinking</span>
          </label>
          <label className="output-filter-option">
            <input
              type="checkbox"
              checked={value.tools}
              onChange={(event) =>
                onChange({ ...value, tools: event.target.checked })
              }
            />
            <span>Tools</span>
          </label>
        </section>
      )}
    </div>
  );
}
