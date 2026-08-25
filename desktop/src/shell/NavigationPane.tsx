export type Route = "chat" | "management" | "workflows" | "settings";
interface NavigationPaneProps {
  readonly route: Route;
  readonly collapsed: boolean;
  readonly onNavigate: (route: Route) => void;
  readonly onNewChat: () => void;
  readonly newChatDisabledReason?: string | null;
  readonly onToggleCollapsed: () => void;
}

/** Persistent desktop navigation in the formal-design order. */
export function NavigationPane({
  route,
  collapsed,
  onNavigate,
  onNewChat,
  newChatDisabledReason = null,
  onToggleCollapsed,
}: NavigationPaneProps): React.JSX.Element {
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
      {!collapsed && <p className="nav-section-label">CHAT</p>}
      <div className="nav-group">
        <NavigationButton
          active={route === "chat"}
          icon="○"
          label="Chat"
          collapsed={collapsed}
          onClick={() => onNavigate("chat")}
        />
      </div>
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
          {!collapsed && (
            <>
              <span>Local desktop</span>
            </>
          )}
        </div>
      </div>
    </nav>
  );
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
