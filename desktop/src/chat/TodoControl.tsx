import { useEffect, useId, useMemo, useRef, useState } from "react";
import type { RuntimeEvent } from "./corePort";
import { currentTodos, todoProgress, todoStatusLabel } from "./todoProjection";
import "./todo.css";

interface Props {
  readonly events: readonly RuntimeEvent[];
}

/**
 * Composer task-list control. The button opens the agent's latest task list in
 * a read-only dialog: the list is agent-owned state that the Run's todo tool
 * writes, so the operator may inspect it at any moment but never edit it here.
 * The control is always present, including before the agent has written one.
 */
export function TodoControl({ events }: Props): React.JSX.Element {
  const todos = useMemo(() => currentTodos(events), [events]);
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const popupId = useId();

  useEffect(() => {
    if (!open) return;
    const dismiss = (event: PointerEvent) => {
      if (
        event.target instanceof Node &&
        !root.current?.contains(event.target)
      ) {
        close(false);
      }
    };
    document.addEventListener("pointerdown", dismiss);
    return () => document.removeEventListener("pointerdown", dismiss);
  }, [open]);

  const progress = todos === null ? null : todoProgress(todos);
  const summary =
    progress === null
      ? "no task list yet"
      : `${progress.done} of ${progress.total} done`;
  const label = `Task list (${summary})`;

  /** Closes the dialog; focus returns to the button unless it is a pointer dismissal. */
  function close(restoreFocus: boolean): void {
    setOpen(false);
    if (restoreFocus) trigger.current?.focus();
  }

  return (
    <div
      ref={root}
      className="todo-control"
      onKeyDown={(event) => {
        if (event.key === "Escape" && open) {
          event.stopPropagation();
          close(true);
        }
      }}
    >
      <button
        ref={trigger}
        className="todo-list-button"
        type="button"
        aria-label={label}
        title={`${label} — click to view the agent's task list`}
        aria-expanded={open}
        aria-controls={popupId}
        aria-haspopup="dialog"
        onClick={() => setOpen(!open)}
      >
        <svg width="23" height="23" viewBox="0 0 24 24" aria-hidden="true">
          {/* A checklist: two ticked rows and one row still open. */}
          <g
            fill="none"
            stroke="currentColor"
            strokeWidth="1.6"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <path d="M4 6.4 5.6 8 8.4 5" />
            <path d="M4 12.4 5.6 14 8.4 11" />
            <path d="M11 6.6h9" />
            <path d="M11 12.6h9" />
            <path d="M11 18.6h9" />
            <path d="M4.2 18.6h1.6" />
          </g>
        </svg>
      </button>
      {open && (
        <section
          id={popupId}
          role="dialog"
          aria-label="Task list"
          className="todo-popover"
        >
          <header className="todo-popover-heading">
            <strong>Task list</strong>
            {progress !== null && (
              <span className="todo-count">
                {progress.done} of {progress.total} done
              </span>
            )}
          </header>
          {todos === null || todos.length === 0 ? (
            <p className="todo-empty">This Chat has no task list yet.</p>
          ) : (
            <ul className="todo-entries">
              {todos.map((todo, index) => (
                <li
                  key={`${index}-${todo.content}`}
                  className={`todo-entry todo-${todo.status}`}
                >
                  <span className="todo-status">
                    {todoStatusLabel(todo.status)}
                  </span>
                  <span className="todo-content">{todo.content}</span>
                </li>
              ))}
            </ul>
          )}
        </section>
      )}
    </div>
  );
}
