/**
 * Per-Chat subagent tab state.
 *
 * Tabs are presentation only: the parent Chat is the permanent first tab and
 * every delegated child is one closable tab identified by its durable childId.
 * The open set is bound to one Chat and remembered for the session, so
 * switching Chats switches tab sets instead of creating a second Run.
 */

/** At most this many subagent tabs stay open at once. */
export const SUBAGENT_TAB_LIMIT = 6;

export interface SubagentTabState {
  /** Open child ids in tab order; the parent Chat is implicit and first. */
  readonly open: readonly string[];
  /** Active subagent child id, or null while the parent Chat tab is active. */
  readonly active: string | null;
}

export const NO_SUBAGENT_TABS: SubagentTabState = { open: [], active: null };

/** Keeps the strip bounded by evicting the oldest non-active tab. */
function bounded(
  open: readonly string[],
  added: string | null,
  active: string | null,
): readonly string[] {
  let next = added === null || open.includes(added) ? [...open] : [...open, added];
  while (next.length > SUBAGENT_TAB_LIMIT) {
    // The active tab is never evicted while another candidate exists; if it is
    // the only candidate the strip is already at its bound for active work.
    const victim = next.findIndex((id) => id !== active);
    if (victim < 0) break;
    next = [...next.slice(0, victim), ...next.slice(victim + 1)];
  }
  return next;
}

function withActive(next: readonly string[], active: string | null): SubagentTabState {
  return { open: next, active: active !== null && next.includes(active) ? active : null };
}

/** Opens a child tab and activates it; an already open child is only activated. */
export function openSubagentTab(
  state: SubagentTabState,
  childId: string,
): SubagentTabState {
  return withActive(bounded(state.open, childId, childId), childId);
}

/**
 * Opens a child tab in the background without changing the active tab. The
 * auto-open preference uses this so a new child never steals focus.
 */
export function openSubagentTabInBackground(
  state: SubagentTabState,
  childId: string,
): SubagentTabState {
  return withActive(bounded(state.open, childId, state.active), state.active);
}

/** Activates an open child, opening it when necessary; null selects the parent. */
export function activateSubagentTab(
  state: SubagentTabState,
  childId: string | null,
): SubagentTabState {
  if (childId === null) {
    return state.active === null ? state : { open: state.open, active: null };
  }
  if (!state.open.includes(childId)) return openSubagentTab(state, childId);
  return state.active === childId ? state : { open: state.open, active: childId };
}

/** Closes a tab. Closing never affects the child; it only hides the tab. */
export function closeSubagentTab(
  state: SubagentTabState,
  childId: string,
): SubagentTabState {
  if (!state.open.includes(childId)) return state;
  const next = state.open.filter((id) => id !== childId);
  return withActive(next, state.active === childId ? null : state.active);
}

/**
 * The auto-close preference: a child that reaches a terminal state closes its
 * tab only when that tab is open and is not the active one.
 */
export function autoCloseSubagentTab(
  state: SubagentTabState,
  childId: string,
): SubagentTabState {
  if (state.active === childId) return state;
  return closeSubagentTab(state, childId);
}

/** Drops remembered tabs whose child the Chat no longer owns. */
export function dropUnknownSubagentTabs(
  state: SubagentTabState,
  known: ReadonlySet<string>,
): SubagentTabState {
  const next = state.open.filter((id) => known.has(id));
  if (next.length === state.open.length) return state;
  return withActive(next, state.active);
}

/** Session memory of each Chat's tab set. Never persisted, never canonical. */
export class SubagentTabMemory {
  private readonly perChat = new Map<string, SubagentTabState>();

  public state(chatId: string): SubagentTabState {
    return this.perChat.get(chatId) ?? NO_SUBAGENT_TABS;
  }

  public set(chatId: string, state: SubagentTabState): void {
    this.perChat.set(chatId, state);
  }

  public clear(): void {
    this.perChat.clear();
  }
}
