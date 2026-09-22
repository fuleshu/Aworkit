import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useNotificationPublisher } from "../notifications/NotificationContext";
import { fitMenuToViewport, type MenuPosition } from "../shell/menuPosition";
import {
  usePathAction,
  type PathActionName,
  type PathActionOutcome,
} from "./pathActions";
import "./pathContextMenu.css";

interface PathActionChoice {
  readonly action: PathActionName;
  readonly label: string;
  readonly title: string;
  readonly failure: string;
}

/** The actions a user may choose; `inspect` only resolves, so it has no entry. */
const PATH_ACTION_CHOICES: readonly PathActionChoice[] = [
  {
    action: "open_default",
    label: "Open in default application",
    title: "Open with your operating system's default application for this format",
    failure: "Could not open the file.",
  },
  {
    action: "open_editor",
    label: "Open in editor",
    title:
      "Open with the editor configured in Settings, or the system default when none is configured",
    failure: "Could not open the file in your editor.",
  },
  {
    action: "reveal",
    label: "Reveal in folder",
    title: "Show this file in your operating system's file manager",
    failure: "Could not reveal the file.",
  },
];

function describeFailure(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  return message.trim() === "" ? "The request failed." : message;
}

/**
 * The menu one path in a conversation offers. It asks the trusted core to
 * resolve the path first, so an action is only offered when the path is really
 * inside the Chat's workspace and still exists; the resolved location is shown
 * before anything is opened. A refusal never reaches the operating system.
 */
export function PathContextMenu({
  path,
  position,
  onClose,
}: {
  /** The path exactly as the conversation showed it. */
  readonly path: string;
  /** Where the menu was requested, in viewport coordinates. */
  readonly position: MenuPosition;
  readonly onClose: () => void;
}): React.JSX.Element | null {
  const run = usePathAction();
  const notifications = useNotificationPublisher("Chat", "path-actions", "chat");
  const menuRef = useRef<HTMLDivElement>(null);
  const [placement, setPlacement] = useState<MenuPosition>(position);
  const [outcome, setOutcome] = useState<PathActionOutcome | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [pending, setPending] = useState<PathActionName | null>(null);

  useEffect(() => {
    if (run === null) return;
    let current = true;
    // Resolving is read-only, so a menu that is closed before it answers simply
    // drops the answer instead of acting on a path the user has left behind.
    void run(path, "inspect").then(
      (resolved) => {
        if (current) setOutcome(resolved);
      },
      (error: unknown) => {
        if (current) setFailure(describeFailure(error));
      },
    );
    return () => {
      current = false;
    };
  }, [path, run]);

  useEffect(() => {
    const dismiss = (event: Event) => {
      const target = event.target;
      if (
        event.type === "pointerdown" &&
        target instanceof Node &&
        menuRef.current?.contains(target) === true
      )
        return;
      onClose();
    };
    window.addEventListener("pointerdown", dismiss);
    window.addEventListener("resize", dismiss);
    window.addEventListener("scroll", dismiss, true);
    const dismissOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", dismissOnEscape);
    return () => {
      window.removeEventListener("pointerdown", dismiss);
      window.removeEventListener("resize", dismiss);
      window.removeEventListener("scroll", dismiss, true);
      window.removeEventListener("keydown", dismissOnEscape);
    };
  }, [onClose]);

  useLayoutEffect(() => {
    const bounds = menuRef.current?.getBoundingClientRect();
    if (bounds === undefined) return;
    const fitted = fitMenuToViewport(position, bounds.width, bounds.height);
    if (fitted.left !== placement.left || fitted.top !== placement.top)
      setPlacement(fitted);
  }, [position, placement.left, placement.top]);

  useEffect(() => {
    menuRef.current?.focus();
  }, []);

  if (run === null) return null;

  const reason =
    outcome === null
      ? null
      : outcome.eligible
        ? null
        : (outcome.reason ?? "This path is not available in the workspace.");
  const ready = outcome?.eligible === true && pending === null;
  const perform = (action: PathActionName) => {
    const choice = PATH_ACTION_CHOICES.find((entry) => entry.action === action);
    setPending(action);
    void run(path, action).then(
      (performed) => {
        setPending(null);
        onClose();
        if (!performed.performed) {
          notifications.publish("path-action", {
            summary: choice?.failure ?? "Could not act on the file.",
            detail: `${path} — ${performed.reason ?? "the action was refused"}`,
            severity: "error",
            lifetime: { kind: "transient" },
          });
        }
      },
      (error: unknown) => {
        setPending(null);
        onClose();
        notifications.publish("path-action", {
          summary: choice?.failure ?? "Could not act on the file.",
          detail: `${path} — ${describeFailure(error)}`,
          severity: "error",
          lifetime: { kind: "transient" },
        });
      },
    );
  };

  return createPortal(
    <div
      aria-label={`Actions for ${outcome?.absolutePath ?? path}`}
      className="path-context-menu"
      ref={menuRef}
      role="menu"
      style={{ left: placement.left, top: placement.top }}
      tabIndex={-1}
    >
      <p className="path-context-menu-target" title={outcome?.absolutePath ?? path}>
        {outcome?.absolutePath ?? path}
      </p>
      {failure !== null ? (
        <p className="path-context-menu-reason" role="alert">
          {failure}
        </p>
      ) : (
        <>
          {PATH_ACTION_CHOICES.map((choice) => (
            <button
              key={choice.action}
              disabled={!ready}
              role="menuitem"
              title={reason ?? choice.title}
              type="button"
              onClick={() => perform(choice.action)}
            >
              {choice.label}
            </button>
          ))}
          {reason !== null && (
            <p className="path-context-menu-reason" role="note">
              {reason}
            </p>
          )}
        </>
      )}
    </div>,
    document.body,
  );
}
