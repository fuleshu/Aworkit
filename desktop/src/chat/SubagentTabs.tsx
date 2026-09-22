import { useRef } from "react";
import type { SubagentCatalogEntry, SubagentStatus } from "./subagentCatalog";
import type { SubagentTabState } from "./subagentTabState";

interface SubagentTabsProps {
  readonly entries: readonly SubagentCatalogEntry[];
  readonly state: SubagentTabState;
  /** Id of the tabpanel whose content the tabs control. */
  readonly panelId: string;
  readonly onActivate: (childId: string | null) => void;
  readonly onClose: (childId: string) => void;
}

export function subagentTabLabel(entry: SubagentCatalogEntry): string {
  const task = entry.task.trim().replace(/\s+/g, " ");
  if (task.length === 0) return entry.childId;
  return task.length > 48 ? `${task.slice(0, 47)}…` : task;
}

export function subagentStatusLabel(status: SubagentStatus): string {
  return status === "parent_approval_required"
    ? "Needs parent approval"
    : status.replace(/^./, (value) => value.toUpperCase());
}

/**
 * The Chat Workspace tab strip: the parent Chat plus one closable tab per open
 * delegated child. It is presentation only — closing a tab hides it and never
 * cancels the child, and each tab is a filtered view of the same Run history.
 */
export function SubagentTabs({
  entries,
  state,
  panelId,
  onActivate,
  onClose,
}: SubagentTabsProps): React.JSX.Element {
  const strip = useRef<HTMLDivElement>(null);
  const open = state.open
    .map((childId) => entries.find((entry) => entry.childId === childId))
    .filter((entry): entry is SubagentCatalogEntry => entry !== undefined);
  const active = state.active;
  const parentSelected = active === null || !open.some((entry) => entry.childId === active);
  // Roving focus follows the rendered order, so arrow navigation can move the
  // active tab without the strip losing track of the focused element.
  const order: (string | null)[] = [null, ...open.map((entry) => entry.childId)];
  const focusTab = (childId: string | null) => {
    const element = strip.current?.querySelector<HTMLButtonElement>(
      `[data-subagent-tab="${childId ?? "__chat__"}"]`,
    );
    element?.focus();
  };
  const move = (from: string | null, delta: number) => {
    const index = order.indexOf(from);
    if (index < 0) return;
    const next = order[(index + delta + order.length) % order.length];
    onActivate(next);
    focusTab(next);
  };
  return (
    <div
      ref={strip}
      className="subagent-tabs"
      role="tablist"
      aria-label="Chat and subagent conversations"
      onKeyDown={(event) => {
        if (event.target instanceof HTMLElement) {
          const group = event.target.closest<HTMLElement>(
            "[data-subagent-tab-group]",
          );
          const owner = event.target.closest<HTMLElement>("[data-subagent-tab]");
          const childId =
            group?.dataset.subagentTabGroup ??
            (owner?.dataset.subagentTab === "__chat__"
              ? null
              : (owner?.dataset.subagentTab ?? null));
          if (event.key === "ArrowRight") {
            event.preventDefault();
            move(childId, 1);
          } else if (event.key === "ArrowLeft") {
            event.preventDefault();
            move(childId, -1);
          } else if (event.key === "Home") {
            event.preventDefault();
            onActivate(null);
            focusTab(null);
          } else if (event.key === "End") {
            const last = open.at(-1)?.childId;
            if (last !== undefined) {
              onActivate(last);
              focusTab(last);
            }
          } else if (
            (event.key === "Delete" || event.key === "Backspace") &&
            childId !== null &&
            open.some((entry) => entry.childId === childId)
          ) {
            event.preventDefault();
            onClose(childId);
            const index = open.findIndex((entry) => entry.childId === childId);
            const neighbour = open[index + 1] ?? open[index - 1];
            const next = neighbour?.childId ?? null;
            onActivate(next);
            focusTab(next);
          }
        }
      }}
    >
      <button
        type="button"
        role="tab"
        id={`${panelId}-tab-chat`}
        data-subagent-tab="__chat__"
        aria-controls={panelId}
        aria-selected={parentSelected}
        tabIndex={parentSelected ? 0 : -1}
        className={`subagent-tab subagent-tab-parent ${parentSelected ? "active" : ""}`}
        title="The parent Chat conversation"
        onClick={() => onActivate(null)}
      >
        Chat
      </button>
      {open.map((entry) => {
        const selected = active === entry.childId;
        return (
          <span
            key={entry.childId}
            role="presentation"
            className={`subagent-tab-group ${selected ? "active" : ""}`}
            data-subagent-tab-group={entry.childId}
          >
            <button
              type="button"
              role="tab"
              id={`${panelId}-tab-${entry.childId}`}
              data-subagent-tab={entry.childId}
              aria-controls={panelId}
              aria-selected={selected}
              tabIndex={selected ? 0 : -1}
              className={`subagent-tab subagent-tab-child ${selected ? "active" : ""}`}
              title={`Subagent ${entry.kind}: ${entry.task}`}
              onClick={() => onActivate(entry.childId)}
            >
              <span
                className={`subagent-status-dot ${entry.status}`}
                aria-hidden="true"
              />
              <span className="subagent-tab-label">
                {subagentTabLabel(entry)}
              </span>
            </button>
            <button
              type="button"
              className="subagent-tab-close"
              aria-label={`Close subagent tab ${subagentTabLabel(entry)}`}
              title="Close this tab. The subagent keeps running."
              onClick={() => onClose(entry.childId)}
            >
              <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true">
                <path
                  d="M2.5 2.5 9.5 9.5M9.5 2.5 2.5 9.5"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="1.6"
                  strokeLinecap="round"
                />
              </svg>
            </button>
          </span>
        );
      })}
    </div>
  );
}
