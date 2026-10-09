import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import { fitMenuToViewport, type MenuPosition } from "../shell/menuPosition";
import type { ChatOutputFilterState } from "./chatOutputFilter";
import "./chatOutputFilter.css";

interface Props {
  readonly value: ChatOutputFilterState;
  readonly onChange: (value: ChatOutputFilterState) => void;
}

/**
 * Header control that chooses which optional Chat output entries are shown.
 *
 * The popover is rendered in a portal because the header's action row is a
 * horizontal scroll container, which would clip an inline popover. The choice
 * is presentation-only session state: it never changes what the Run records,
 * and flipping a box re-renders the already-rendered transcript.
 */
export function ChatOutputFilter({ value, onChange }: Props): React.JSX.Element {
  const [open, setOpen] = useState(false);
  const [placement, setPlacement] = useState<MenuPosition | null>(null);
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const popup = useRef<HTMLElement>(null);
  const anchor = useRef<{ right: number; bottom: number } | null>(null);
  const popupId = useId();
  const hidden = [
    !value.thinking && "reasoning",
    !value.tools && "tools",
  ].filter((entry): entry is string => entry !== false);
  const label =
    hidden.length === 0
      ? "Chat output filter"
      : `Chat output filter (hiding ${hidden.join(" and ")})`;

  const close = useCallback((restoreFocus: boolean) => {
    setOpen(false);
    setPlacement(null);
    if (restoreFocus) trigger.current?.focus();
  }, []);

  const show = () => {
    const bounds = trigger.current?.getBoundingClientRect();
    if (bounds === undefined) return;
    anchor.current = { right: bounds.right, bottom: bounds.bottom };
    setPlacement({ left: bounds.right, top: bounds.bottom + 4 });
    setOpen(true);
  };

  useEffect(() => {
    if (!open) return;
    const dismiss = (event: PointerEvent) => {
      const target = event.target;
      if (
        target instanceof Node &&
        (root.current?.contains(target) || popup.current?.contains(target))
      ) {
        return;
      }
      close(false);
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        close(true);
      }
    };
    // A moved anchor would strand the portal, so any scroll or resize closes it.
    const reposition = () => close(false);
    document.addEventListener("pointerdown", dismiss);
    document.addEventListener("keydown", escape);
    window.addEventListener("resize", reposition);
    window.addEventListener("scroll", reposition, true);
    return () => {
      document.removeEventListener("pointerdown", dismiss);
      document.removeEventListener("keydown", escape);
      window.removeEventListener("resize", reposition);
      window.removeEventListener("scroll", reposition, true);
    };
  }, [open, close]);

  // Align the popover's right edge with the trigger once its real size is known,
  // then keep it inside the viewport.
  useLayoutEffect(() => {
    const bounds = popup.current?.getBoundingClientRect();
    if (!open || bounds === undefined || anchor.current === null) return;
    const fitted = fitMenuToViewport(
      {
        left: anchor.current.right - bounds.width,
        top: anchor.current.bottom + 4,
      },
      bounds.width,
      bounds.height,
    );
    if (fitted.left !== placement?.left || fitted.top !== placement?.top) {
      setPlacement(fitted);
    }
  }, [open, placement]);

  return (
    <div ref={root} className="output-filter">
      <button
        ref={trigger}
        className="output-filter-button"
        type="button"
        aria-label={label}
        aria-expanded={open}
        aria-controls={popupId}
        aria-haspopup="dialog"
        title={`${label} — choose which Chat entries are shown`}
        onClick={() => (open ? close(true) : show())}
      >
        <svg width="16" height="16" viewBox="0 0 24 24" aria-hidden="true">
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
      {open &&
        placement !== null &&
        createPortal(
          <section
            ref={popup}
            id={popupId}
            role="dialog"
            aria-label="Chat output filter"
            className="output-filter-popover"
            style={{ left: placement.left, top: placement.top }}
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
          </section>,
          document.body,
        )}
    </div>
  );
}
