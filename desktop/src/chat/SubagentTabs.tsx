import { useMemo, useRef } from "react";
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

/**
 * The full, task-derived label used where there is room to read it (the picker
 * dialog and every tooltip). The strip itself uses a short label.
 */
export function subagentTabLabel(entry: SubagentCatalogEntry): string {
  const task = entry.task.trim().replace(/\s+/g, " ");
  if (task.length === 0) return entry.childId;
  return task.length > 48 ? `${task.slice(0, 47)}…` : task;
}

/**
 * The product name of one external delegation, when its generated context
 * prefix names it. A missing or unrecognized product falls back to `External`,
 * so the tab never claims a product it cannot identify.
 */
function externalProductLabel(contextText: string): string {
  const match = /^External\s+([A-Za-z0-9_-]+)\s+agent\./.exec(
    contextText.trim(),
  );
  if (match === null) return "External";
  const backend = match[1];
  if (backend === "codex") return "Codex";
  if (backend === "claude-code") return "Claude Code";
  return backend.replace(/^./, (value) => value.toUpperCase());
}

/** The short kind name a tab uses before its creation-order number. */
export function subagentKindLabel(entry: SubagentCatalogEntry): string {
  if (entry.kind === "fork") return "Fork";
  if (entry.kind === "external") return externalProductLabel(entry.contextText);
  return "Subagent";
}

/** Creation time of a tab label order; unparseable stamps sort first. */
function createdStamp(entry: SubagentCatalogEntry): number {
  const parsed = entry.createdAt === undefined ? NaN : Date.parse(entry.createdAt);
  return Number.isNaN(parsed) ? 0 : parsed;
}

/**
 * Short, stable tab labels: the kind (or product) plus the child's 1-based
 * creation-order number within that kind. Numbering follows creation order, so
 * a running child is not renumbered when a sibling settles, and the full task
 * stays available in the tab's tooltip.
 */
export function subagentTabLabels(
  entries: readonly SubagentCatalogEntry[],
): Map<string, string> {
  const ordered = [...entries].sort(
    (left, right) =>
      createdStamp(left) - createdStamp(right) ||
      left.childId.localeCompare(right.childId),
  );
  const counters = new Map<string, number>();
  const labels = new Map<string, string>();
  for (const entry of ordered) {
    const kind = subagentKindLabel(entry);
    const next = (counters.get(kind) ?? 0) + 1;
    counters.set(kind, next);
    labels.set(entry.childId, `${kind} ${next}`);
  }
  return labels;
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
 *
 * The strip exists only for delegated children. With no open child tab the
 * parent Chat is the whole workspace, so a lone "Chat" tab is not rendered —
 * a settled child that left the catalog must not leave an empty strip behind.
 */
export function SubagentTabs({
  entries,
  state,
  panelId,
  onActivate,
  onClose,
}: SubagentTabsProps): React.JSX.Element | null {
  const strip = useRef<HTMLDivElement>(null);
  // Short, stable labels for every child in the catalog; computed before the
  // early return so hook order never changes.
  const labels = useMemo(() => subagentTabLabels(entries), [entries]);
  const open = state.open
    .map((childId) => entries.find((entry) => entry.childId === childId))
    .filter((entry): entry is SubagentCatalogEntry => entry !== undefined);
  if (open.length === 0) return null;
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
        <span className="subagent-tab-label">Chat</span>
      </button>
      {open.map((entry) => {
        const selected = active === entry.childId;
        const short = labels.get(entry.childId) ?? subagentTabLabel(entry);
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
              title={`${short} · subagent ${entry.kind}: ${entry.task}`}
              onClick={() => onActivate(entry.childId)}
            >
              <span
                className={`subagent-status-dot ${entry.status}`}
                aria-hidden="true"
              />
              <span className="subagent-tab-label">{short}</span>
            </button>
            <button
              type="button"
              className="subagent-tab-close"
              aria-label={`Close subagent tab ${short}`}
              title={`Close ${short}. The subagent keeps running.`}
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
