import { useEffect, useMemo, useRef, useState } from "react";
import type { ChatHistoryEntry, ChatProjectChoice } from "../chat/types";

export type Route = "chat" | "management" | "workflows" | "settings";
interface NavigationPaneProps {
  readonly route: Route;
  readonly collapsed: boolean;
  readonly onNavigate: (route: Route) => void;
  readonly onNewChat: () => void;
  readonly newChatDisabledReason?: string | null;
  readonly onToggleCollapsed: () => void;
  readonly history?: readonly ChatHistoryEntry[];
  readonly projects?: readonly ChatProjectChoice[];
  readonly selectedChatId?: string | null;
  readonly historyDisabledReason?: string | null;
  readonly onSelectChat?: (chatId: string) => void;
  readonly onSetChatPinned?: (chatId: string, pinned: boolean) => void;
  readonly onForkChat?: (chatId: string) => void;
  readonly onDeleteChat?: (chatId: string) => void;
}

interface ProjectHistoryGroup {
  readonly id: string;
  readonly name: string;
  readonly entries: readonly ChatHistoryEntry[];
}

/** Persistent desktop navigation in the formal-design order. */
export function NavigationPane({
  route,
  collapsed,
  onNavigate,
  onNewChat,
  newChatDisabledReason = null,
  onToggleCollapsed,
  history = [],
  projects = [],
  selectedChatId = null,
  historyDisabledReason = null,
  onSelectChat,
  onSetChatPinned,
  onForkChat,
  onDeleteChat,
}: NavigationPaneProps): React.JSX.Element {
  const organized = useMemo(
    () => organizeHistory(history, projects),
    [history, projects],
  );
  return (
    <nav
      className={`navigation-pane ${collapsed ? "collapsed" : ""}`}
      aria-label="Primary navigation"
    >
      <div className="brand-row">
        <span className="brand-mark">A</span>
        {!collapsed && <strong>Aworkit</strong>}
        <button
          aria-label={collapsed ? "Expand navigation" : "Collapse navigation"}
          title={
            collapsed
              ? "Expand navigation"
              : "Collapse navigation to a compact rail"
          }
          type="button"
          onClick={onToggleCollapsed}
        >
          {collapsed ? "›" : "‹"}
        </button>
      </div>
      <button
        className="new-chat"
        disabled={newChatDisabledReason !== null}
        title={newChatDisabledReason ?? "Create a new Chat"}
        type="button"
        onClick={onNewChat}
      >
        <span>＋</span>
        {!collapsed && "New Chat"}
      </button>
      <NavigationButton
        active={false}
        icon="●"
        label="Management Chat — Unsupported"
        collapsed={collapsed}
        disabled
        disabledTitle="Management Chat is unsupported in this build"
      />
      <NavigationButton
        active={route === "workflows"}
        icon="◇"
        label="Workflows"
        collapsed={collapsed}
        onClick={() => onNavigate("workflows")}
      />
      <NavigationButton
        active={false}
        icon="◷"
        label="Schedules"
        collapsed={collapsed}
        disabled
      />
      {collapsed ? (
        <NavigationButton
          active={route === "chat"}
          icon="○"
          label="Chat history"
          collapsed
          onClick={() => onNavigate("chat")}
        />
      ) : (
        <div className="navigation-history" aria-label="Chat history">
          {organized.pinned.length > 0 && (
            <HistorySection
              label="PINNED"
              entries={organized.pinned}
              route={route}
              selectedChatId={selectedChatId}
              disabledReason={historyDisabledReason}
              onSelectChat={onSelectChat}
              onSetChatPinned={onSetChatPinned}
              onForkChat={onForkChat}
              onDeleteChat={onDeleteChat}
            />
          )}
          {organized.projects.map((group) => (
            <details className="history-project" key={group.id} open>
              <summary title={`Show or hide Chats for ${group.name}`}>
                <span aria-hidden="true">▱</span>
                <span>{group.name}</span>
              </summary>
              <div className="history-project-entries">
                {group.entries.map((entry) => (
                  <ChatHistoryRow
                    key={entry.chatId}
                    entry={entry}
                    active={route === "chat" && selectedChatId === entry.chatId}
                    disabledReason={historyDisabledReason}
                    onSelectChat={onSelectChat}
                    onSetChatPinned={onSetChatPinned}
                    onForkChat={onForkChat}
                    onDeleteChat={onDeleteChat}
                  />
                ))}
              </div>
            </details>
          ))}
          {organized.standalone.length > 0 && (
            <HistorySection
              label="CHATS"
              entries={organized.standalone}
              route={route}
              selectedChatId={selectedChatId}
              disabledReason={historyDisabledReason}
              onSelectChat={onSelectChat}
              onSetChatPinned={onSetChatPinned}
              onForkChat={onForkChat}
              onDeleteChat={onDeleteChat}
            />
          )}
          {history.length === 0 && (
            <div className="nav-group">
              <p className="nav-section-label">CHAT</p>
              <NavigationButton
                active={route === "chat"}
                icon="○"
                label="Chat"
                collapsed={false}
                onClick={() => onNavigate("chat")}
              />
            </div>
          )}
        </div>
      )}
      <div className="navigation-footer">
        <NavigationButton
          active={route === "settings"}
          icon="⚙"
          label="Settings"
          collapsed={collapsed}
          onClick={() => onNavigate("settings")}
        />
        <div className="account-row">
          <span className="avatar">L</span>
          {!collapsed && <span>Local desktop</span>}
        </div>
      </div>
    </nav>
  );
}

function HistorySection({
  label,
  entries,
  route,
  selectedChatId,
  disabledReason,
  onSelectChat,
  onSetChatPinned,
  onForkChat,
  onDeleteChat,
}: {
  readonly label: string;
  readonly entries: readonly ChatHistoryEntry[];
  readonly route: Route;
  readonly selectedChatId: string | null;
  readonly disabledReason: string | null;
  readonly onSelectChat?: (chatId: string) => void;
  readonly onSetChatPinned?: (chatId: string, pinned: boolean) => void;
  readonly onForkChat?: (chatId: string) => void;
  readonly onDeleteChat?: (chatId: string) => void;
}): React.JSX.Element {
  return (
    <section className="history-section" aria-label={label.toLowerCase()}>
      <p className="nav-section-label">{label}</p>
      {entries.map((entry) => (
        <ChatHistoryRow
          key={entry.chatId}
          entry={entry}
          active={route === "chat" && selectedChatId === entry.chatId}
          disabledReason={disabledReason}
          onSelectChat={onSelectChat}
          onSetChatPinned={onSetChatPinned}
          onForkChat={onForkChat}
          onDeleteChat={onDeleteChat}
        />
      ))}
    </section>
  );
}

function ChatHistoryRow({
  entry,
  active,
  disabledReason,
  onSelectChat,
  onSetChatPinned,
  onForkChat,
  onDeleteChat,
}: {
  readonly entry: ChatHistoryEntry;
  readonly active: boolean;
  readonly disabledReason: string | null;
  readonly onSelectChat?: (chatId: string) => void;
  readonly onSetChatPinned?: (chatId: string, pinned: boolean) => void;
  readonly onForkChat?: (chatId: string) => void;
  readonly onDeleteChat?: (chatId: string) => void;
}): React.JSX.Element {
  const [menuOpen, setMenuOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);
  const disabled = disabledReason !== null || onSelectChat === undefined;
  useEffect(() => {
    if (!menuOpen) return;
    const close = (event: PointerEvent) => {
      if (!menuRef.current?.contains(event.target as Node)) setMenuOpen(false);
    };
    window.addEventListener("pointerdown", close);
    return () => window.removeEventListener("pointerdown", close);
  }, [menuOpen]);
  const invoke = (action: () => void) => {
    setMenuOpen(false);
    action();
  };
  return (
    <div
      className={`chat-history-row ${active ? "active" : ""}`}
      data-chat-id={entry.chatId}
    >
      <button
        aria-current={active ? "page" : undefined}
        className="chat-history-link"
        disabled={disabled}
        title={disabledReason ?? `Open ${entry.title}`}
        type="button"
        onClick={() => onSelectChat?.(entry.chatId)}
        onContextMenu={(event) => {
          event.preventDefault();
          if (disabledReason === null) setMenuOpen(true);
        }}
      >
        <span className="nav-icon" aria-hidden="true">○</span>
        <span>{entry.title}</span>
      </button>
      <div className="chat-history-actions" ref={menuRef}>
        <button
          aria-expanded={menuOpen}
          aria-haspopup="menu"
          aria-label={`Actions for ${entry.title}`}
          className="chat-history-more"
          disabled={disabledReason !== null}
          title={disabledReason ?? `Chat actions for ${entry.title}`}
          type="button"
          onClick={() => setMenuOpen((open) => !open)}
        >
          ⋯
        </button>
        {menuOpen && (
          <div
            className="chat-history-menu"
            role="menu"
            onKeyDown={(event) => {
              if (event.key === "Escape") setMenuOpen(false);
            }}
          >
            <button
              role="menuitem"
              title={entry.pinned ? "Move this Chat back into its history group" : "Keep this Chat in the Pinned section"}
              type="button"
              onClick={() => invoke(() => onSetChatPinned?.(entry.chatId, !entry.pinned))}
            >
              {entry.pinned ? "Unpin" : "Pin"}
            </button>
            <button
              role="menuitem"
              title="Create a new Chat with this conversation as its parent"
              type="button"
              onClick={() => invoke(() => onForkChat?.(entry.chatId))}
            >
              Fork
            </button>
            <button
              className="danger-menu-item"
              role="menuitem"
              title="Delete this Chat from history"
              type="button"
              onClick={() => invoke(() => onDeleteChat?.(entry.chatId))}
            >
              Delete
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

function organizeHistory(
  history: readonly ChatHistoryEntry[],
  projects: readonly ChatProjectChoice[],
): {
  readonly pinned: readonly ChatHistoryEntry[];
  readonly projects: readonly ProjectHistoryGroup[];
  readonly standalone: readonly ChatHistoryEntry[];
} {
  const pinned = history.filter((entry) => entry.pinned);
  const ordinary = history.filter((entry) => !entry.pinned);
  const projectNames = new Map(projects.map((project) => [project.projectId, project.name]));
  const grouped = new Map<string, ChatHistoryEntry[]>();
  for (const entry of ordinary) {
    if (entry.projectId === null) continue;
    const existing = grouped.get(entry.projectId) ?? [];
    existing.push(entry);
    grouped.set(entry.projectId, existing);
  }
  const projectGroups = [...grouped.entries()]
    .map(([id, entries]) => ({
      id,
      name: projectNames.get(id) ?? entries[0]?.projectName ?? "Unavailable project",
      entries,
    }))
    .sort((left, right) => left.name.localeCompare(right.name));
  return {
    pinned,
    projects: projectGroups,
    standalone: ordinary.filter((entry) => entry.projectId === null),
  };
}

function NavigationButton({
  active,
  icon,
  label,
  collapsed,
  disabled,
  disabledTitle,
  nested,
  strong,
  onClick,
}: {
  readonly active: boolean;
  readonly icon: string;
  readonly label: string;
  readonly collapsed: boolean;
  readonly disabled?: boolean;
  readonly disabledTitle?: string;
  readonly nested?: boolean;
  readonly strong?: boolean;
  readonly onClick?: () => void;
}): React.JSX.Element {
  return (
    <button
      aria-current={active ? "page" : undefined}
      className={`${nested ? "nested" : ""} ${strong ? "strong" : ""}`}
      disabled={disabled || onClick === undefined}
      title={
        collapsed
          ? label
          : disabled
            ? (disabledTitle ?? `${label} is unsupported in this build`)
            : label
      }
      type="button"
      onClick={onClick}
    >
      <span className="nav-icon">{icon}</span>
      {!collapsed && <span>{label}</span>}
    </button>
  );
}
