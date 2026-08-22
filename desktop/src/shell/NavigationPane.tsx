export type Route = "chat" | "management" | "workflows" | "settings";
interface NavigationPaneProps {
  readonly route: Route;
  readonly collapsed: boolean;
  readonly onNavigate: (route: Route) => void;
  readonly onNewChat: () => void;
  readonly onToggleCollapsed: () => void;
}

/** Persistent desktop navigation in the formal-design order. */
export function NavigationPane({
  route,
  collapsed,
  onNavigate,
  onNewChat,
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
        title="Create a new Chat"
        type="button"
        onClick={onNewChat}
      >
        <span>＋</span>
        {!collapsed && "New Chat"}
      </button>
      <NavigationButton
        active={route === "management"}
        icon="●"
        label="Management Chat"
        collapsed={collapsed}
        onClick={() => onNavigate("management")}
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
      {!collapsed && <p className="nav-section-label">PROJECTS</p>}
      <div className="nav-group">
        <NavigationButton
          active={false}
          icon="⌄"
          label="Project Atlas"
          collapsed={collapsed}
          strong
        />
        <NavigationButton
          active={route === "chat"}
          icon=""
          label="Release readiness"
          collapsed={collapsed}
          nested
          onClick={() => onNavigate("chat")}
        />
        <NavigationButton
          active={false}
          icon=""
          label="API migration"
          collapsed={collapsed}
          nested
        />
        <NavigationButton
          active={false}
          icon="›"
          label="Research Lab"
          collapsed={collapsed}
          strong
        />
      </div>
      {!collapsed && <p className="nav-section-label">HISTORY</p>}
      <div className="nav-group">
        <NavigationButton
          active={false}
          icon=""
          label="Local model setup"
          collapsed={collapsed}
          nested
        />
        <NavigationButton
          active={false}
          icon=""
          label="Compare note tools"
          collapsed={collapsed}
          nested
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
          <span className="avatar">T</span>
          {!collapsed && (
            <>
              <span>Tim</span>
              <i title="Trusted core connected" />
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
  nested,
  strong,
  onClick,
}: {
  readonly active: boolean;
  readonly icon: string;
  readonly label: string;
  readonly collapsed: boolean;
  readonly disabled?: boolean;
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
          : `${label}${disabled ? " is not available in this milestone" : ""}`
      }
      type="button"
      onClick={onClick}
    >
      <span className="nav-icon">{icon}</span>
      {!collapsed && <span>{label}</span>}
    </button>
  );
}
