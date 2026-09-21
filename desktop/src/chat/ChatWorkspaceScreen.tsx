import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import { ChatBusy } from "./ChatBusy";
import { conversationFeed } from "./conversationFeed";
import "./chatLoading.css";
import "./subagents.css";
import { PaneSplitter } from "../shell/PaneSplitter";
import { usePaneWidth } from "../shell/usePaneWidth";
import { useProjectedNotification } from "../notifications/NotificationContext";
import {
  hasOpenSemanticSpan,
  projectSemanticTimeline,
} from "./activityProjection";
import type { ChatCorePort, RuntimeSnapshot } from "./corePort";
import {
  ChatComposer,
  type WorkflowOption,
} from "./ChatComposer";
import { ConversationTimeline } from "./ConversationTimeline";
import { ApprovalModeSelect } from "./ApprovalModeSelect";
import { ContextUsage } from "./ContextUsage";
import { GoalControl } from "./GoalControl";
import { SubagentConversation } from "./SubagentConversation";
import { SubagentDialog } from "./SubagentDialog";
import { SubagentTabs } from "./SubagentTabs";
import {
  chatSubagentCatalog,
  subagentEntry,
  type SubagentCatalogEntry,
} from "./subagentCatalog";
import {
  autoCloseSubagentTab,
  activateSubagentTab,
  closeSubagentTab,
  openSubagentTab,
  openSubagentTabInBackground,
  SubagentTabMemory,
  type SubagentTabState,
} from "./subagentTabs";
import { useSubagentFeed } from "./useSubagentFeed";
import {
  DEFAULT_SUBAGENT_VIEW,
  type SubagentViewPreference,
} from "./subagentViewPreference";
import { withEventSupport } from "./eventWindow";
import { useContextModel } from "./useContextModel";
import type { ApprovalActionDetails } from "./approvals";
import { controlsFor } from "./composer";
import { RunDetailsInspector } from "./RunDetailsInspector";
import { ChatWorkspaceController } from "./workspace";
import { useChatRuntime } from "./useChatRuntime";
import { ComposerDrafts } from "./composerDrafts";
import { useChatErrorNotices } from "./useChatErrorNotices";
import type { ChatIntent, TimelineItem } from "./types";
import {
  createWorkflowLibraryPort,
  TauriWorkflowCorePort,
  type WorkflowCorePort,
  type WorkflowLibraryPort,
} from "../workbench/corePort";

interface ChatWorkspaceScreenProps {
  readonly corePort?: ChatCorePort;
  readonly pollIntervalMs?: number;
  readonly newChatRequest?: number;
  readonly historyActionRequest?: ChatHistoryActionRequest | null;
  readonly active?: boolean;
  readonly onReveal?: (after: () => void) => void;
  readonly workflowPort?: Pick<WorkflowCorePort, "snapshot">;
  readonly libraryPort?: WorkflowLibraryPort;
  /** Bumped by any surface that changed the stored workflow library. */
  readonly libraryRevision?: number;
  readonly onRecoveryPendingChange?: (pending: boolean) => void;
  readonly onRuntimeSnapshotChange?: (
    snapshot: RuntimeSnapshot,
    state: { readonly stale: boolean; readonly pending: boolean },
  ) => void;
  readonly confirmRecoveryAbandon?: (
    title: string,
    body: string,
  ) => Promise<boolean>;
  /** Persisted Run-details separator position, applied once when it arrives. */
  readonly storedInspectorWidth?: number;
  readonly onInspectorWidthChange?: (width: number) => void;
  /** Global delegated-subagent tab presentation preference. */
  readonly subagentView?: SubagentViewPreference;
}

export interface ChatHistoryActionRequest {
  readonly requestId: number;
  readonly type: "select_chat" | "set_chat_pinned" | "delete_chat" | "fork";
  readonly targetId: string;
  readonly pinned?: boolean;
}

/** Complete projected Chat surface connected to the native trusted-core adapter. */
export function ChatWorkspaceScreen({
  corePort,
  pollIntervalMs,
  newChatRequest = 0,
  historyActionRequest = null,
  active = true,
  onReveal,
  workflowPort,
  libraryPort,
  libraryRevision = 0,
  onRecoveryPendingChange,
  onRuntimeSnapshotChange,
  confirmRecoveryAbandon = browserRecoveryConfirmation,
  storedInspectorWidth,
  onInspectorWidthChange,
  subagentView = DEFAULT_SUBAGENT_VIEW,
}: ChatWorkspaceScreenProps): React.JSX.Element {
  const runtime = useChatRuntime(corePort, pollIntervalMs);
  const commandIds = useMemo(() => new ChatWorkspaceController(), []);
  const composerDrafts = useMemo(() => new ComposerDrafts(), []);
  const contextSave = useRef<{ fingerprint: string; intent: ChatIntent; version: number } | null>(null);
  const [inspectorOpen, setInspectorOpen] = useState(true);
  const inspector = usePaneWidth(320, 280, 420, storedInspectorWidth);
  const { width: inspectorWidth, setWidth: setInspectorWidth } = inspector;
  const chatLayoutRef = useRef<HTMLElement>(null);
  const inspectorRef = inspector.ref;
  const attachChatLayout = useCallback((element: HTMLElement | null) => {
    chatLayoutRef.current = element;
    inspectorRef(element);
  }, [inspectorRef]);
  const previewInspectorWidth = useCallback((width: number) => {
    chatLayoutRef.current?.style.setProperty(
      "--aw-inspector-width",
      `${width}px`,
    );
  }, []);
  const [selectedTimelineId, setSelectedTimelineId] = useState<string | null>(null);
  // A stopped Run is reported persistently in the workspace, not only as a
  // transient notice the user can miss. The banner clears when a later turn
  // succeeds or the user dismisses it, never silently.
  const [dismissedRunFailure, setDismissedRunFailure] = useState<string | null>(null);
  const nativeWorkflowPort = useMemo(
    () =>
      workflowPort ??
      ("__TAURI_INTERNALS__" in window ? new TauriWorkflowCorePort() : null),
    [workflowPort],
  );
  const [workflows, setWorkflows] = useState<readonly WorkflowOption[]>([]);
  const [defaultWorkflowId, setDefaultWorkflowId] = useState<string | null>(null);
  const [selectedWorkflowId, setSelectedWorkflowId] = useState<string | null>(
    null,
  );
  const [workflowChecking, setWorkflowChecking] = useState(nativeWorkflowPort !== null);
  const [workflowReadinessError, setWorkflowReadinessError] = useState<
    string | null
  >(null);
  const [confirmingRecoveryAbandon, setConfirmingRecoveryAbandon] =
    useState(false);
  const [stopPending, setStopPending] = useState(false);
  const handledNewChatRequest = useRef(0);
  const handledHistoryActionRequest = useRef(0);
  const wasActive = useRef(active);
  const chatPanelId = useId();
  const snapshot = runtime.snapshot;
  const projectedRecoveryPending = snapshot?.chat.recoveryPending;
  const projectedChatId = snapshot?.chat.chatId;
  const resolvedContextModel = useContextModel(runtime.contextModel, projectedChatId,
    snapshot?.chat.workflowId ?? selectedWorkflowId, snapshot?.contextModel, active);
  const timelineItems = useMemo(
    () => projectSemanticTimeline(runtime.events),
    [runtime.events],
  );
  const feedItems = useMemo(() => conversationFeed(timelineItems, runtime.firstSequence), [timelineItems, runtime.firstSequence]);
  // Delegated children: the durable frame catalog merged with the live facts,
  // and the per-Chat tab set that filters the same Run history for one child.
  const subagentTabs = useMemo(() => new SubagentTabMemory(), []);
  const [, setSubagentTabRevision] = useState(0);
  const tabChatId = projectedChatId ?? "";
  const tabState = subagentTabs.state(tabChatId);
  const updateTabs = useCallback(
    (update: (state: SubagentTabState) => SubagentTabState) => {
      if (tabChatId === "") return;
      subagentTabs.set(tabChatId, update(subagentTabs.state(tabChatId)));
      setSubagentTabRevision((revision) => revision + 1);
    },
    [subagentTabs, tabChatId],
  );
  const subagentEntries = useMemo(
    () => chatSubagentCatalog(snapshot?.subagents, runtime.events),
    [snapshot?.subagents, runtime.events],
  );
  const activeChild =
    tabState.active === null
      ? null
      : (subagentEntry(subagentEntries, tabState.active) ?? null);
  const observedChildren = useRef<{ chatId: string; seen: Set<string> }>({
    chatId: "",
    seen: new Set(),
  });
  const childStatuses = useRef<{ chatId: string; statuses: Map<string, string> }>({
    chatId: "",
    statuses: new Map(),
  });
  useEffect(() => {
    if (tabChatId === "") return;
    if (observedChildren.current.chatId !== tabChatId) {
      // Returning to a Chat rebuilds its remembered tab set; only children
      // created while this workspace observes the Chat may auto-open.
      observedChildren.current = {
        chatId: tabChatId,
        seen: new Set(subagentEntries.map((entry) => entry.childId)),
      };
      return;
    }
    const seen = observedChildren.current.seen;
    const created = subagentEntries.filter((entry) => !seen.has(entry.childId));
    for (const entry of created) seen.add(entry.childId);
    if (!subagentView.autoOpen || created.length === 0) return;
    updateTabs((state) =>
      created.reduce(
        (current, entry) => openSubagentTabInBackground(current, entry.childId),
        state,
      ),
    );
  }, [subagentEntries, subagentView.autoOpen, tabChatId, updateTabs]);
  useEffect(() => {
    if (tabChatId === "") return;
    if (childStatuses.current.chatId !== tabChatId) {
      childStatuses.current = {
        chatId: tabChatId,
        statuses: new Map(
          subagentEntries.map((entry) => [entry.childId, entry.status]),
        ),
      };
      return;
    }
    const statuses = childStatuses.current.statuses;
    const terminal: string[] = [];
    for (const entry of subagentEntries) {
      const previous = statuses.get(entry.childId);
      statuses.set(entry.childId, entry.status);
      if (
        subagentView.autoClose &&
        previous === "running" &&
        entry.status !== "running"
      ) {
        terminal.push(entry.childId);
      }
    }
    if (terminal.length === 0) return;
    updateTabs((state) =>
      terminal.reduce(
        (current, childId) => autoCloseSubagentTab(current, childId),
        state,
      ),
    );
  }, [subagentEntries, subagentView.autoClose, tabChatId, updateTabs]);
  const childFeed = useSubagentFeed(
    runtime.port,
    tabChatId,
    activeChild?.childId ?? "",
    snapshot?.throughSequence ?? 0,
    runtime.events,
    active && activeChild !== null,
  );
  const childItems = useMemo(
    () =>
      activeChild === null
        ? []
        : conversationFeed(
            projectSemanticTimeline(
              withEventSupport(childFeed.events, childFeed.support),
            ),
            childFeed.firstSequence,
          ),
    [activeChild, childFeed.events, childFeed.firstSequence, childFeed.support],
  );
  const openChildTab = useCallback(
    (childId: string) => updateTabs((state) => openSubagentTab(state, childId)),
    [updateTabs],
  );
  const childForCall = useCallback(
    (callId: string) =>
      subagentEntries.find((entry) => entry.parentCallId === callId)?.childId,
    [subagentEntries],
  );
  // A tab switch is a different conversation scope; the Run-details selection
  // belongs to the tab it was made in.
  useEffect(() => {
    setSelectedTimelineId(null);
  }, [tabState.active]);
  const liveTurnRunning = useMemo(
    () => snapshot !== null && hasOpenSemanticSpan(runtime.events)
      && (snapshot.chat.phase !== "awaiting_approval" || runtime.pendingCommandIds.size > 0),
    [runtime.events, snapshot, runtime.pendingCommandIds.size],
  );
  const inspect = () => {
    const reveal = () => { setSelectedTimelineId(null); setInspectorOpen(true); };
    if (onReveal) onReveal(reveal); else reveal();
  };
  useChatErrorNotices(
    runtime.events,
    snapshot !== null,
    projectedChatId ?? null,
    runtime.stale ? null : runtime.error,
    runtime.pendingCommandIds.size > 0,
    inspect,
  );
  useProjectedNotification("Chat", "chat-connection", "connection", !runtime.stale ? null : {
    route: "chat", summary: "Projection disconnected.", detail: runtime.error?.message ?? "The last known state remains visible. Changes are disabled until resynchronized.", severity: "warning", lifetime: { kind: "condition", conditionId: "chat-projection" },
    action: { label: "Resync", disabled: runtime.pendingCommandIds.size > 0, run: () => void runtime.resynchronize() },
  });
  useProjectedNotification("Chat", "chat-recovery", "recovery", !projectedRecoveryPending ? null : {
    route: "chat", summary: "Interrupted command requires an explicit decision.", severity: "action", lifetime: { kind: "condition", conditionId: `chat-recovery:${projectedChatId}` },
    action: { label: "Review", run: () => { const reveal = () => chatLayoutRef.current?.querySelector<HTMLButtonElement>(".recovery-actions button")?.focus(); if (onReveal) onReveal(reveal); else reveal(); } },
  });
  useProjectedNotification("Chat", `chat:${projectedChatId ?? "startup"}`, "command", runtime.stale || runtime.pendingCommandIds.size === 0 ? null : {
    route: "chat", summary: "Waiting for the Chat command to commit…", severity: "progress", lifetime: { kind: "operation", operationId: [...runtime.pendingCommandIds].join(":") },
  });

  // The saved-workflow library is re-read whenever this surface becomes active
  // or another surface reports a change. A workflow created or renamed after
  // this Chat mounted must still appear in the composer's Workflow list.
  useEffect(() => {
    if (!active) return;
    const port: WorkflowLibraryPort =
      libraryPort ?? createWorkflowLibraryPort();
    let current = true;
    void port
      .snapshot()
      .then((library) => {
        if (!current) return;
        const entries = library.entries.map((entry) => ({
          id: entry.id,
          name: entry.name,
        }));
        setWorkflows(entries);
        setDefaultWorkflowId(library.defaultWorkflowId);
        setSelectedWorkflowId((currentId) =>
          currentId !== null &&
          entries.some((entry) => entry.id === currentId)
            ? currentId
            : library.defaultWorkflowId,
        );
      })
      .catch((failure: unknown) => {
        if (current)
          // Keep the last known entries usable; the failure is reported instead
          // of blanking a projectable list.
          setWorkflowReadinessError(
            failure instanceof Error
              ? `Could not load the workflow library: ${failure.message}`
              : "Could not load the workflow library.",
          );
      });
    return () => {
      current = false;
    };
  }, [libraryPort, libraryRevision, active]);
  useEffect(() => {
    const reentered = active && !wasActive.current;
    wasActive.current = active;
    if (reentered) void runtime.resynchronize();
  }, [active, runtime.resynchronize]);
  useEffect(() => {
    if (!active) return;
    if (nativeWorkflowPort === null || selectedWorkflowId === null) {
      setWorkflowChecking(false);
      setWorkflowReadinessError(null);
      return;
    }
    let current = true;
    setWorkflowChecking(true);
    setWorkflowReadinessError(null);
    void nativeWorkflowPort
      .snapshot(selectedWorkflowId)
      .then(({ editable }) => {
        if (!current) return;
        if (!editable) {
          setWorkflowChecking(false);
          setWorkflowReadinessError(
            "The selected workflow uses a read-only schema and cannot run.",
          );
          return;
        }
        setWorkflowChecking(false);
      })
      .catch(() => {
        if (current) {
          setWorkflowChecking(false);
          setWorkflowReadinessError(
            "The selected workflow could not be checked; resynchronize before sending.",
          );
        }
      });
    return () => {
      current = false;
    };
  }, [active, nativeWorkflowPort, selectedWorkflowId]);
  useEffect(() => {
    if (projectedRecoveryPending !== undefined)
      onRecoveryPendingChange?.(projectedRecoveryPending);
  }, [onRecoveryPendingChange, projectedRecoveryPending]);
  useEffect(() => {
    setSelectedTimelineId(null);
    setStopPending(false);
  }, [projectedChatId]);
  useEffect(() => {
    if (!liveTurnRunning) setStopPending(false);
  }, [liveTurnRunning]);
  useEffect(() => {
    if (
      newChatRequest <= handledNewChatRequest.current ||
      runtime.snapshot === null
    )
      return;
    handledNewChatRequest.current = newChatRequest;
    setSelectedTimelineId(null);
    void runtime.dispatch(commandIds.createIntent("new_chat"));
  }, [commandIds, newChatRequest, runtime]);
  useEffect(() => {
    if (
      historyActionRequest === null ||
      historyActionRequest.requestId <= handledHistoryActionRequest.current ||
      runtime.snapshot === null
    )
      return;
    handledHistoryActionRequest.current = historyActionRequest.requestId;
    const created = commandIds.createIntent(
      historyActionRequest.type,
      historyActionRequest.targetId,
    );
    const intent: ChatIntent =
      created.type === "set_chat_pinned"
        ? { ...created, pinned: historyActionRequest.pinned ?? false }
        : created;
    void runtime.dispatch(intent);
  }, [commandIds, historyActionRequest, runtime]);
  useEffect(() => {
    if (runtime.snapshot !== null)
      onRuntimeSnapshotChange?.(runtime.snapshot, {
        stale: runtime.stale,
        pending: runtime.pendingCommandIds.size > 0,
      });
  }, [
    onRuntimeSnapshotChange,
    runtime.pendingCommandIds.size,
    runtime.snapshot,
    runtime.stale,
  ]);
  if (runtime.loading && snapshot === null)
    return (
      <>
        <section className="chat-layout"><main className="chat-main"><ChatBusy label="Opening your chats…" /></main></section>
      </>
    );
  if (snapshot === null)
    return (
      <>
        <section className="route-error" role="alert">
          <h2>Chat projection unavailable</h2>
          <button
            type="button"
            title="Retry the trusted-core projection query"
            onClick={() => void runtime.resynchronize()}
          >
            Retry
          </button>
        </section>
      </>
    );
  const chat = snapshot.chat;
  const runFailure = (() => {
    const failed = runtime.events.filter(
      (event) => event.kind === "execution.failed" && event.eventId !== dismissedRunFailure,
    );
    const latest = failed.at(-1);
    if (latest === undefined) return null;
    const payload = latest.payload as { title?: unknown; body?: unknown };
    return {
      id: latest.eventId,
      title: typeof payload.title === "string" ? payload.title : "The Run stopped.",
      body:
        typeof payload.body === "string"
          ? payload.body
          : "The trusted core recorded this failure. Inspect Run details for the exact record.",
    };
  })();
  const dismissRunFailure = (id: string) => setDismissedRunFailure(id);
  const visibleChat = liveTurnRunning
    ? { ...chat, phase: "running" as const }
    : chat;
  const control = (type: "cancel") => {
    // Stop is the safety control for a live turn: it must stay usable while the
    // model is working, including while the projection is stale, and it must not
    // latch itself off when a slow turn ignores the first cancellation. A stale
    // projection is resynchronized first so the cancel carries a current fence.
    setStopPending(true);
    void (async () => {
      try {
        if (runtime.stale) await runtime.resynchronize();
        await runtime.dispatch(commandIds.createIntent(type, chat.chatId));
      } finally {
        setStopPending(false);
      }
    })();
  };
  const cardAction = (
    action: NonNullable<TimelineItem["action"]>,
    targetId: string,
    details?: ApprovalActionDetails,
  ) => {
    const intent = timelineActionIntent(
      action,
      targetId,
      commandIds.createIntent(
        action === "approve" || action === "reject" ? "approval" : action,
      ).commandId,
      details,
    );
    void runtime.dispatch(intent);
  };
  const selectTimelineItem = (id: string | null) => {
    setSelectedTimelineId(id);
    setInspectorOpen(true);
  };
  const chatContext = [
    chat.workflowName,
    chat.branch,
    chat.runId === "run.draft" ? null : chat.runId,
  ].filter((item): item is string => item !== null);
  return (
    <section
      ref={attachChatLayout}
      className={`chat-layout ${inspectorOpen ? "with-inspector" : ""}`}
      style={
        inspectorOpen
          ? ({
              "--aw-inspector-width": `${inspectorWidth}px`,
            } as React.CSSProperties)
          : undefined
      }
    >
      <main className="chat-main">
        <header className="chat-view-header">
          <div>
            <p className="eyebrow">{chat.scope.toUpperCase()}</p>
            <div className="chat-title-line">
              <h1>{chat.title}</h1>
              {chatContext.length > 0 && <span>{chatContext.join(" · ")}</span>}
            </div>
          </div>
          <div className="run-actions">
            <button
              aria-pressed={inspectorOpen}
              title="Show or hide Run details"
              type="button"
              onClick={() => setInspectorOpen((open) => !open)}
            >
              Run details
            </button>
          </div>
        </header>
        {chat.recoveryPending ? (
          <div className="recovery-banner" role="status">
            <div>
              <strong>Choose how to recover this command.</strong>
              <p>
                Aworkit preserved the exact staged command. Resume recovers that
                command. You can open other Chats or create a New Chat while this one awaits recovery.
              </p>
            </div>
            <div className="recovery-actions">
              {runtime.stale && (
                <button
                  title="Request a fresh trusted-core snapshot before recovery"
                  type="button"
                  onClick={() => void runtime.resynchronize()}
                >
                  Resync
                </button>
              )}
              <button
                className="primary-action"
                disabled={
                  runtime.stale ||
                  runtime.pendingCommandIds.size > 0 ||
                  confirmingRecoveryAbandon
                }
                title={
                  runtime.stale
                    ? "Resynchronize before resuming the interrupted command"
                    : runtime.pendingCommandIds.size > 0
                      ? "A recovery command is awaiting a committed core event"
                      : confirmingRecoveryAbandon
                        ? "Finish the recovery-abandonment confirmation first"
                      : "Replay the exact staged interrupted command with a fresh idempotent resume command ID"
                }
                type="button"
                onClick={() =>
                  void runtime.dispatch(commandIds.createIntent("resume"))
                }
              >
                Resume interrupted command
              </button>
              <button
                className="danger-action"
                disabled={
                  runtime.stale ||
                  runtime.pendingCommandIds.size > 0 ||
                  confirmingRecoveryAbandon
                }
                title={
                  runtime.stale
                    ? "Resynchronize before abandoning the interrupted command"
                    : runtime.pendingCommandIds.size > 0
                      ? "A recovery command is awaiting a committed core event"
                      : "Record the interrupted command as outcome-uncertain without replaying its provider or tool effects"
                }
                type="button"
                onClick={() => {
                  setConfirmingRecoveryAbandon(true);
                  void confirmRecoveryAbandon(
                    "Abandon interrupted command as uncertain?",
                    "Aworkit will record an explicit outcome-uncertain failure and evidence for the original staged command without calling its provider or tools. This cannot determine whether effects occurred before the interruption.",
                  )
                    .then((confirmed) => {
                      if (confirmed)
                        return runtime.dispatch(
                          commandIds.createIntent("abandon_recovery"),
                        );
                      return false;
                    })
                    .finally(() => setConfirmingRecoveryAbandon(false));
                }}
              >
                Abandon as uncertain
              </button>
            </div>
          </div>
        ) : null}
        {runFailure !== null ? (
          <div className="recovery-banner run-failure-banner" role="alert">
            <div>
              <strong>{runFailure.title}</strong>
              <p>{runFailure.body}</p>
            </div>
            <div className="recovery-actions">
              <button type="button" onClick={inspect}>
                Run details
              </button>
              <button type="button" onClick={() => dismissRunFailure(runFailure.id)}>
                Dismiss
              </button>
            </div>
          </div>
        ) : null}
        {(subagentEntries.length > 0 || activeChild !== null) && (
          <SubagentTabs
            entries={subagentEntries}
            state={tabState}
            panelId={chatPanelId}
            onActivate={(childId) =>
              updateTabs((state) => activateSubagentTab(state, childId))
            }
            onClose={(childId) =>
              updateTabs((state) => closeSubagentTab(state, childId))
            }
          />
        )}
        <div
          className="chat-tabpanel"
          id={chatPanelId}
          role="tabpanel"
          tabIndex={-1}
          aria-labelledby={
            activeChild === null
              ? `${chatPanelId}-tab-chat`
              : `${chatPanelId}-tab-${activeChild.childId}`
          }
        >
          {runtime.loading ? (
            <ChatBusy />
          ) : activeChild !== null ? (
            <SubagentConversation
              entry={activeChild}
              items={childItems}
              selectedId={null}
              onSelect={noopSelect}
              hasOlder={childFeed.hasOlder}
              olderLoading={childFeed.olderLoading}
              olderError={childFeed.olderError}
              loading={childFeed.loading}
              onLoadOlder={childFeed.loadOlder}
              active={active}
            />
          ) : (
            <ConversationTimeline
              key={chat.chatId}
              active={active}
              items={feedItems}
              selectedId={selectedTimelineId}
              actionsDisabled={runtime.pendingCommandIds.size > 0}
              onSelect={selectTimelineItem}
              onAction={cardAction}
              subagentForCall={childForCall}
              onOpenSubagent={openChildTab}
              hasOlder={runtime.hasOlderEvents}
              olderLoading={runtime.olderLoading}
              olderError={runtime.olderError}
              onLoadOlder={runtime.loadOlder}
            />
          )}
        </div>
        {activeChild === null && (
          <ChatComposer
            drafts={composerDrafts}
            committedEvents={runtime.events}
            key={chat.chatId + (defaultWorkflowId ?? "")}
            subagentsControl={
              <SubagentDialog
                entries={subagentEntries}
                onOpenChild={openChildTab}
              />
            }
            approvalControl={<ApprovalModeSelect compact value={chat.approvalMode ?? "ask_for_approval"}
            disabled={runtime.stale || runtime.pendingCommandIds.size > 0 || liveTurnRunning || chat.recoveryPending}
            onChange={mode => void runtime.dispatch({ type: "approval_mode", commandId: commandIds.createIntent("approval_mode").commandId, targetId: chat.chatId, mode })} />}
          status={<span role="status" className={`run-status ${visibleChat.phase}`}><i />{label(visibleChat.phase)}</span>}
          onStop={controlsFor(visibleChat).includes("cancel") ? () => control("cancel") : undefined}
          stopDisabled={chat.recoveryPending}
          stopRequested={stopPending}
            chat={{...chat,queuedInputs:[...chat.queuedInputs,...runtime.queuedMaintenanceInputs]}}
          goalControl={<GoalControl events={runtime.events}
            disabledReason={runtime.stale ? "Resynchronize before changing the goal."
              : chat.recoveryPending ? "Resume or abandon the interrupted turn before changing the goal."
              : null}
            onSubmit={goal => runtime.dispatch({
              type: "set_goal", commandId: commandIds.createIntent("set_goal").commandId, targetId: chat.chatId, goal,
            })} />}
          contextUsage={<ContextUsage events={runtime.events} model={resolvedContextModel}
            onCompact={selection => runtime.dispatch({type:"compact_context", commandId:commandIds.createIntent("enqueue").commandId,targetId:chat.chatId,nodeId:selection.nodeId,baseSequence:selection.sequence})}
            editDisabledReason={runtime.stale ? "Resynchronize before editing context."
              : chat.recoveryPending ? "Resume or abandon the interrupted turn before editing context."
              : runtime.pendingCommandIds.size > 0 || liveTurnRunning || chat.phase === "awaiting_approval"
                ? "Finish or stop the current turn to enable editing." : null}
            onSave={(selection, document) => {
              const fingerprint = JSON.stringify([chat.chatId, selection.nodeId, selection.sequence, document]);
              if (contextSave.current?.fingerprint !== fingerprint) contextSave.current = {
                fingerprint, version: snapshot.version, intent: {
                  type: "edit_context", commandId: commandIds.createIntent("enqueue").commandId, targetId: chat.chatId,
                  nodeId: selection.nodeId, baseSequence: selection.sequence, document,
                },
              };
              return runtime.dispatch(contextSave.current.intent, contextSave.current.version);
            }} />}
          projects={snapshot.projects}
          stale={runtime.stale}
            pending={runtime.loading || ((runtime.pendingCommandIds.size > 0 || snapshot.activeChatIds?.includes(chat.chatId) === true) && !runtime.maintenancePending && runtime.queuedMaintenanceInputs.length === 0)}
          workflows={workflows}
          defaultWorkflowId={defaultWorkflowId}
          workflowChecking={workflowChecking}
          workflowReadinessError={workflowReadinessError}
          nextCommandId={() => commandIds.createIntent("enqueue").commandId}
          onWorkflowChange={setSelectedWorkflowId}
          onSubmit={runtime.dispatch}
        />
        )}
      </main>
      {inspectorOpen && (
        <PaneSplitter
          className="inspector-splitter"
          direction={-1}
          label="Resize Run details"
          max={inspector.max}
          min={280}
          value={inspectorWidth}
          onPreview={previewInspectorWidth}
          onChange={(width) => {
            setInspectorWidth(width);
            onInspectorWidthChange?.(width);
          }}
        />
      )}
      {inspectorOpen && (
        <RunDetailsInspector
          partial={runtime.hasOlderEvents}
          chat={visibleChat}
          events={runtime.events}
          items={timelineItems}
          records={snapshot.evidence}
          selectedId={selectedTimelineId}
          onSelect={selectTimelineItem}
          onClose={() => setInspectorOpen(false)}
        />
      )}
    </section>
  );
}

export function timelineActionIntent(
  action: NonNullable<TimelineItem["action"]>,
  targetId: string,
  commandId: string,
  details?: import("./approvals").ApprovalActionDetails,
): ChatIntent {
  return action === "approve" || action === "reject"
    ? {
        type: "approval",
        commandId,
        decisionId: targetId,
        approved: action === "approve",
        ...(details ?? {}),
      }
    : { type: action, commandId, targetId };
}

/** A child tab is read-only: it never selects parent Run details. */
function noopSelect(): void {}

function label(phase: string): string {
  if (phase === "waiting_input" || phase === "draft") return "Waiting for input";
  return phase
    .replaceAll("_", " ")
    .replace(/^./, (value) => value.toUpperCase());
}

async function browserRecoveryConfirmation(
  _title: string,
  body: string,
): Promise<boolean> {
  return window.confirm(body);
}
