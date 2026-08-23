import { lazy, Suspense, useEffect, useRef, useState } from "react";
import type { DesktopAdapters } from "./adapters/contracts";
import {
  nativePresentationEvent,
  type NativePresentationRequest,
} from "./adapters/contracts";
import { ChatWorkspaceScreen } from "./chat/ChatWorkspaceScreen";
import type { ManagementRepairCorePort } from "./management/corePort";
import { ManagementScreen } from "./shell/ManagementScreen";
import { NavigationPane, type Route } from "./shell/NavigationPane";
import { PaneSplitter } from "./shell/PaneSplitter";
const SettingsScreen = lazy(() =>
  import("./workbench/SettingsScreen").then((module) => ({
    default: module.SettingsScreen,
  })),
);
const WorkflowEditorScreen = lazy(() =>
  import("./workbench/WorkflowEditorScreen").then((module) => ({
    default: module.WorkflowEditorScreen,
  })),
);

interface AppProps {
  readonly adapters: DesktopAdapters;
  /** Explicit test/story seam; production resolves the native core adapter. */
  readonly managementRepairCorePort?: ManagementRepairCorePort;
}
const starterWorkflow = {
  schemaVersion: 1,
  name: "Repository Engineer",
  nodes: [
    {
      id: "input.1",
      label: "Request",
      type: "input",
      position: { x: 36, y: 205 },
    },
    {
      id: "model.fast",
      label: "Fast model",
      type: "model",
      position: { x: 245, y: 112 },
      requirement: "model.fast",
      capabilityStatus: "ready",
    },
    {
      id: "model.quality",
      label: "Quality model",
      type: "model",
      position: { x: 245, y: 296 },
      requirement: "model.quality",
      capabilityStatus: "ready",
    },
    {
      id: "plugin.review",
      label: "acme.code-review@2.x",
      type: "plugin",
      position: { x: 470, y: 112 },
      requirement: "plugin.code-review@2.x",
      capabilityStatus: "missing",
    },
    {
      id: "gate.1",
      label: "Review approval",
      type: "gate",
      position: { x: 470, y: 296 },
      required: true,
    },
    {
      id: "output.1",
      label: "Response",
      type: "output",
      position: { x: 710, y: 205 },
    },
  ],
  edges: [
    { id: "request-fast", source: "input.1", target: "model.fast" },
    {
      id: "fast-review",
      source: "model.fast",
      target: "plugin.review",
    },
    {
      id: "request-quality",
      source: "input.1",
      target: "model.quality",
      label: "deep review",
    },
    {
      id: "review-gate",
      source: "plugin.review",
      target: "gate.1",
    },
    { id: "quality-gate", source: "model.quality", target: "gate.1" },
    { id: "gate-response", source: "gate.1", target: "output.1" },
  ],
  unknownExtension: { retain: true },
  comments: "Unknown fields stay lossless.",
} as const;

/** Persistent compact desktop workbench. Feature views own no canonical state. */
export function App({ adapters, managementRepairCorePort }: AppProps): React.JSX.Element {
  const [route, setRoute] = useState<Route>("chat");
  const [mountedRoutes, setMountedRoutes] = useState<ReadonlySet<Route>>(
    new Set(["chat"]),
  );
  const [navigationWidth, setNavigationWidth] = useState(208);
  const [collapsed, setCollapsed] = useState(false);
  const [newChatRequest, setNewChatRequest] = useState(0);
  const [notification, setNotification] = useState<Extract<
    NativePresentationRequest,
    { kind: "notification" }
  > | null>(null);
  const [confirmation, setConfirmation] = useState<Extract<
    NativePresentationRequest,
    { kind: "confirmation" }
  > | null>(null);
  const mainRef = useRef<HTMLElement>(null);
  const navigate = (next: Route) => {
    setMountedRoutes((current) => new Set([...current, next]));
    setRoute(next);
    window.requestAnimationFrame(() => mainRef.current?.focus());
  };
  const openNewChat = () => {
    navigate("chat");
    setNewChatRequest((request) => request + 1);
  };
  useEffect(() => {
    const shortcut = (event: KeyboardEvent) => {
      if (!(event.ctrlKey || event.metaKey) || event.altKey) return;
      const routes: Record<string, Route> = {
        "1": "chat",
        "2": "workflows",
        ",": "settings",
      };
      const next = routes[event.key];
      if (next !== undefined) {
        event.preventDefault();
        navigate(next);
      }
    };
    window.addEventListener("keydown", shortcut);
    return () => window.removeEventListener("keydown", shortcut);
  }, []);
  useEffect(() => {
    if (!("__TAURI_INTERNALS__" in window)) return;
    let dispose: (() => void) | undefined;
    void import("@tauri-apps/api/event")
      .then(({ listen }) =>
        listen<string>("aworkit:native-menu", ({ payload }) => {
          const actions: Record<string, () => void> = {
            "aworkit.new-chat": openNewChat,
            "aworkit.open-workflow": () => navigate("workflows"),
            "aworkit.settings": () => navigate("settings"),
            "aworkit.chat": () => navigate("chat"),
            "aworkit.workflows": () => navigate("workflows"),
            "aworkit.management": () => navigate("management"),
            "aworkit.shortcuts": () =>
              setNotification({
                kind: "notification",
                title: "Keyboard shortcuts",
                body: "Use Ctrl/Command+1 for Chat, +2 for Workflows, and +, for Settings.",
              }),
          };
          actions[payload]?.();
        }),
      )
      .then((unlisten) => {
        dispose = unlisten;
      })
      .catch(() => {
        // Browser and denied-native-event runs keep keyboard navigation.
      });
    return () => dispose?.();
  }, []);
  useEffect(() => {
    const receive = (event: Event) => {
      const request = (event as CustomEvent<NativePresentationRequest>).detail;
      if (request.kind === "notification") setNotification(request);
      else setConfirmation(request);
    };
    window.addEventListener(nativePresentationEvent, receive);
    return () => window.removeEventListener(nativePresentationEvent, receive);
  }, []);
  return (
    <div
      className="desktop-shell"
      style={{
        gridTemplateColumns: `${collapsed ? 44 : navigationWidth}px 6px minmax(0, 1fr)`,
      }}
    >
      <a className="skip-link" href="#main-surface">
        Skip to main surface
      </a>
      <NavigationPane
        route={route}
        collapsed={collapsed}
        onNavigate={navigate}
        onNewChat={openNewChat}
        onToggleCollapsed={() => setCollapsed((value) => !value)}
      />
      <PaneSplitter
        value={navigationWidth}
        min={184}
        max={264}
        onChange={setNavigationWidth}
      />
      <section
        id="main-surface"
        className="main-surface"
        ref={mainRef}
        tabIndex={-1}
      >
        <Suspense
          fallback={<div className="route-loading">Loading surface…</div>}
        >
          {mountedRoutes.has("chat") && (
            <div className="route-surface" hidden={route !== "chat"}>
              <ChatWorkspaceScreen newChatRequest={newChatRequest} />
            </div>
          )}
          {mountedRoutes.has("workflows") && (
            <div className="route-surface" hidden={route !== "workflows"}>
              <WorkflowEditorScreen
                document={starterWorkflow}
                onOpenSettings={() => navigate("settings")}
                onRun={openNewChat}
              />
            </div>
          )}
          {mountedRoutes.has("settings") && (
            <div className="route-surface" hidden={route !== "settings"}>
              <SettingsScreen />
            </div>
          )}
          {mountedRoutes.has("management") && (
            <div className="route-surface" hidden={route !== "management"}>
              <ManagementScreen
                confirmDecision={(title, body) =>
                  adapters.nativePresentation.confirm(title, body)
                }
                corePort={managementRepairCorePort}
              />
            </div>
          )}
        </Suspense>
      </section>
      <span className="adapter-status" hidden>
        {adapters.components.name} · {adapters.graph.name} ·{" "}
        {adapters.collections.name}
      </span>
      {notification !== null && (
        <section className="workbench-notification" role="status">
          <div>
            <strong>{notification.title}</strong>
            <p>{notification.body}</p>
          </div>
          <button
            aria-label="Dismiss notification"
            title="Dismiss notification"
            type="button"
            onClick={() => setNotification(null)}
          >
            ×
          </button>
        </section>
      )}
      {confirmation !== null && (
        <ConfirmationDialog
          request={confirmation}
          onDecision={(accepted) => {
            confirmation.resolve(accepted);
            setConfirmation(null);
          }}
        />
      )}
    </div>
  );
}

function ConfirmationDialog({
  request,
  onDecision,
}: {
  readonly request: Extract<
    NativePresentationRequest,
    { kind: "confirmation" }
  >;
  readonly onDecision: (accepted: boolean) => void;
}): React.JSX.Element {
  const dialogRef = useRef<HTMLElement>(null);
  const confirmRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    const previous =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;
    confirmRef.current?.focus();
    return () => previous?.focus();
  }, []);
  return (
    <div className="dialog-backdrop">
      <section
        aria-labelledby="confirmation-title"
        aria-modal="true"
        className="workbench-dialog"
        ref={dialogRef}
        role="dialog"
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            onDecision(false);
            return;
          }
          if (event.key !== "Tab") return;
          const controls = Array.from(
            dialogRef.current?.querySelectorAll<HTMLElement>(
              'button:not(:disabled), [href], input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex]:not([tabindex="-1"])',
            ) ?? [],
          );
          const first = controls[0];
          const last = controls.at(-1);
          if (first === undefined || last === undefined) return;
          if (event.shiftKey && document.activeElement === first) {
            event.preventDefault();
            last.focus();
          } else if (!event.shiftKey && document.activeElement === last) {
            event.preventDefault();
            first.focus();
          }
        }}
      >
        <h2 id="confirmation-title">{request.title}</h2>
        <p>{request.body}</p>
        <div>
          <button type="button" onClick={() => onDecision(false)}>
            Cancel
          </button>
          <button
            className="primary-action"
            ref={confirmRef}
            type="button"
            onClick={() => onDecision(true)}
          >
            Confirm
          </button>
        </div>
      </section>
    </div>
  );
}
