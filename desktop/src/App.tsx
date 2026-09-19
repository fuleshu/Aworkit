import {
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import type { DesktopAdapters } from "./adapters/contracts";
import {
  nativePresentationEvent,
  type NativePresentationRequest,
} from "./adapters/contracts";
import {
  ChatWorkspaceScreen,
  type ChatHistoryActionRequest,
} from "./chat/ChatWorkspaceScreen";
import type { RuntimeSnapshot } from "./chat/corePort";
import type { ManagementRepairCorePort } from "./management/corePort";
import { ManagementScreen } from "./shell/ManagementScreen";
import { NavigationPane, type Route } from "./shell/NavigationPane";
import { PaneSplitter } from "./shell/PaneSplitter";
import {
  createDesktopLayoutPort,
  type DesktopLayout,
} from "./shell/desktopLayout";
import { usePaneWidth } from "./shell/usePaneWidth";
import { useSettingsNavigation } from "./shell/settingsNavigation";
import { NotificationStore } from "./notifications/NotificationStore";
import { NotificationProvider } from "./notifications/NotificationContext";
import { DesktopStatusBar } from "./notifications/DesktopStatusBar";
import { bundledDefaultWorkflow } from "./workbench/bundledWorkflows";
import { createWorkflowLibraryPort } from "./workbench/corePort";
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
const starterWorkflow = bundledDefaultWorkflow;

/** Persistent compact desktop workbench. Feature views own no canonical state. */
export function App({ adapters, managementRepairCorePort }: AppProps): React.JSX.Element {
  const [store] = useState(() => new NotificationStore());
  useEffect(() => () => store.dispose(), [store]);
  return <NotificationProvider store={store}><DesktopApp adapters={adapters} managementRepairCorePort={managementRepairCorePort} store={store} /></NotificationProvider>;
}

function DesktopApp({ adapters, managementRepairCorePort, store }: AppProps & { readonly store: NotificationStore }): React.JSX.Element {
  const mainRef = useRef<HTMLElement>(null);
  const { route, mountedRoutes, visit, navigate, back, registerLeaveGuard, returnLabel } = useSettingsNavigation(mainRef, store);
  const workflowLibraryPort = useMemo(() => createWorkflowLibraryPort(), []);
  // A refresh signal, not a second copy of the library: any surface that
  // changed stored workflows tells the others to re-read the native projection.
  const [libraryRevision, setLibraryRevision] = useState(0);
  const noteWorkflowLibraryChange = useCallback(
    () => setLibraryRevision((revision) => revision + 1),
    [],
  );
  const layoutPort = useMemo(() => createDesktopLayoutPort(), []);
  const [desktopLayout, setDesktopLayout] = useState<DesktopLayout>({});
  const navigation = usePaneWidth(208, 184, 640, desktopLayout.historyPaneWidth);
  const { width: navigationWidth, setWidth: setNavigationWidth } = navigation;
  // The native host restores the frame. The renderer hydrates only separators;
  // initial/default widths must never be written over the saved preferences.
  useEffect(() => {
    let current = true;
    void layoutPort
      .snapshot()
      .then((layout) => {
        if (current) setDesktopLayout(layout);
      })
      .catch((error) => console.error("Could not load desktop layout", error));
    return () => {
      current = false;
    };
  }, [layoutPort]);
  const [collapsed, setCollapsed] = useState(false);
  const [newChatRequest, setNewChatRequest] = useState(0);
  const [historyActionRequest, setHistoryActionRequest] =
    useState<ChatHistoryActionRequest | null>(null);
  const historyActionSequence = useRef(0);
  const [chatRuntimeState, setChatRuntimeState] = useState<{
    readonly snapshot: RuntimeSnapshot;
    readonly stale: boolean;
    readonly pending: boolean;
  } | null>(null);
  const [chatRecoveryPending, setChatRecoveryPending] = useState<
    boolean | null
  >(null);
  const setNotification = useCallback((request: Extract<NativePresentationRequest, { kind: "notification" }>) => {
    store.publish(`native:${request.title}`, "application", store.nextOccurrence(), {
      summary: request.title, detail: request.body, source: "Aworkit", severity: "info", lifetime: { kind: "transient" },
    });
  }, [store]);
  const [confirmation, setConfirmation] = useState<Extract<
    NativePresentationRequest,
    { kind: "confirmation" }
  > | null>(null);
  const openNewChat = useCallback(() => {
    navigate("chat", () => setNewChatRequest((request) => request + 1));
  }, [navigate]);
  const requestHistoryAction = useCallback(
    (
      type: ChatHistoryActionRequest["type"],
      targetId: string,
      pinned?: boolean,
    ) => {
      historyActionSequence.current += 1;
      setHistoryActionRequest({
        requestId: historyActionSequence.current,
        type,
        targetId,
        pinned,
      });
    },
    [],
  );
  const updateChatRuntimeState = useCallback(
    (
      snapshot: RuntimeSnapshot,
      state: { readonly stale: boolean; readonly pending: boolean },
    ) => setChatRuntimeState({ snapshot, ...state }),
    [],
  );
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
  }, [navigate]);
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
            "aworkit.management": () =>
              setNotification({
                kind: "notification",
                title: "Management Chat unavailable",
                body: "Management Chat is unsupported in this rescue build.",
              }),
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
  }, [navigate, openNewChat, setNotification]);
  useEffect(() => {
    const receive = (event: Event) => {
      const request = (event as CustomEvent<NativePresentationRequest>).detail;
      if (request.kind === "notification") setNotification(request);
      else setConfirmation(request);
    };
    window.addEventListener(nativePresentationEvent, receive);
    return () => window.removeEventListener(nativePresentationEvent, receive);
  }, [setNotification]);
  useEffect(() => {
    if (route !== "settings") return;
    const escape = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented || event.isComposing) return;
      if ([...document.querySelectorAll('[role="dialog"], [role="alertdialog"], [role="menu"], [aria-haspopup][aria-expanded="true"]')]
        .some(element => !element.closest("[hidden]"))) return;
      event.preventDefault();
      back();
    };
    window.addEventListener("keydown", escape);
    return () => window.removeEventListener("keydown", escape);
  }, [route, back]);
  return (
    <div
      className="desktop-shell"
      ref={navigation.ref}
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
        newChatDisabledReason={
          chatRuntimeState === null ? "Loading Chats" : null
        }
        onToggleCollapsed={() => setCollapsed((value) => !value)}
        history={chatRuntimeState?.snapshot.history.map(entry => ({ ...entry, busy: chatRuntimeState.snapshot.activeChatIds?.includes(entry.chatId) ?? false }))}
        projects={chatRuntimeState?.snapshot.projects}
        selectedChatId={chatRuntimeState?.snapshot.chat.chatId}
        historyDisabledReason={
          chatRuntimeState === null
            ? "Loading Chat history"
            : null
        }
        onSelectChat={(chatId) => {
          navigate("chat", () => {
            if (chatId !== chatRuntimeState?.snapshot.chat.chatId) requestHistoryAction("select_chat", chatId);
          });
        }}
        onSetChatPinned={(chatId, pinned) =>
          requestHistoryAction("set_chat_pinned", chatId, pinned)
        }
        onForkChat={(chatId) => {
          navigate("chat", () => requestHistoryAction("fork", chatId));
        }}
        onDeleteChat={(chatId) => {
          const entry = chatRuntimeState?.snapshot.history.find(
            (candidate) => candidate.chatId === chatId,
          );
          void adapters.nativePresentation
            .confirm(
              "Delete Chat?",
              `Delete “${entry?.title ?? "this Chat"}” from Chat history? Its canonical record will be tombstoned and no longer shown.`,
            )
            .then((confirmed) => {
              if (!confirmed) return;
              navigate("chat", () => requestHistoryAction("delete_chat", chatId));
            });
        }}
      />
      <PaneSplitter
        value={navigationWidth}
        min={184}
        max={navigation.max}
        onPreview={(width) => {
          const shell = mainRef.current?.parentElement;
          if (shell) shell.style.gridTemplateColumns = `${collapsed ? 44 : width}px 6px minmax(0, 1fr)`;
        }}
        onChange={(width) => {
          setNavigationWidth(width);
          // A finished drag is the settle point; committing per frame would write
          // the document dozens of times for one gesture.
          void layoutPort
            .commit({ historyPaneWidth: width })
            .catch((error) => setNotification({ kind: "notification", title: "Could not save desktop layout", body: String(error) }));
        }}
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
            <div className="route-surface" data-route="chat" hidden={route !== "chat"}>
              <ChatWorkspaceScreen
                active={route === "chat"}
                onReveal={after => navigate("chat", after)}
                confirmRecoveryAbandon={(title, body) =>
                  adapters.nativePresentation.confirm(title, body)
                }
                newChatRequest={newChatRequest}
                historyActionRequest={historyActionRequest}
                libraryPort={workflowLibraryPort}
                libraryRevision={libraryRevision}
                onRecoveryPendingChange={setChatRecoveryPending}
                onRuntimeSnapshotChange={updateChatRuntimeState}
                storedInspectorWidth={desktopLayout.inspectorPaneWidth}
                onInspectorWidthChange={(width) => {
                  void layoutPort
                    .commit({ inspectorPaneWidth: width })
                    .catch((error) => setNotification({ kind: "notification", title: "Could not save desktop layout", body: String(error) }));
                }}
              />
            </div>
          )}
          {mountedRoutes.has("workflows") && (
            <div className="route-surface" data-route="workflows" hidden={route !== "workflows"}>
              <WorkflowEditorScreen
                active={route === "workflows"}
                document={starterWorkflow}
                libraryPort={workflowLibraryPort}
                onLibraryChange={noteWorkflowLibraryChange}
                onOpenSettings={() => navigate("settings")}
                onRun={openNewChat}
                runBlockedReason={
                  chatRecoveryPending === null
                    ? "Checking interrupted-command recovery state before starting a Run"
                    : chatRecoveryPending
                      ? "Resume or abandon the interrupted command before starting another Run"
                      : undefined
                }
              />
            </div>
          )}
          {mountedRoutes.has("settings") && (
            <div className="route-surface" data-route="settings" hidden={route !== "settings"}>
              <SettingsScreen presentation={adapters.nativePresentation} active={route === "settings"} visit={visit} onBack={back} returnLabel={returnLabel} registerLeaveGuard={registerLeaveGuard} />
            </div>
          )}
          {mountedRoutes.has("management") && (
            <div className="route-surface" data-route="management" hidden={route !== "management"}>
              <ManagementScreen
                active={route === "management"}
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
      <DesktopStatusBar store={store} route={route} />
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
