import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { PaneSplitter } from "../shell/PaneSplitter";
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
import type { ApprovalActionDetails } from "./approvals";
import { controlsFor } from "./composer";
import { RunDetailsInspector } from "./RunDetailsInspector";
import { ChatWorkspaceController } from "./workspace";
import { useChatRuntime } from "./useChatRuntime";
import { useChatErrorNotices } from "./useChatErrorNotices";
import type { ChatIntent, TimelineItem } from "./types";
import {
  createWorkflowLibraryPort,
  TauriWorkflowCorePort,
  type WorkflowCorePort,
  type WorkflowLibraryPort,
} from "../workbench/corePort";
import { bindsProjectTools } from "../workbench/workflowExecution";

interface ChatWorkspaceScreenProps {
  readonly corePort?: ChatCorePort;
  readonly pollIntervalMs?: number;
  readonly newChatRequest?: number;
  readonly historyActionRequest?: ChatHistoryActionRequest | null;
  readonly active?: boolean;
  readonly onReveal?: (after: () => void) => void;
  readonly workflowPort?: Pick<WorkflowCorePort, "snapshot">;
  readonly libraryPort?: WorkflowLibraryPort;
  readonly onRecoveryPendingChange?: (pending: boolean) => void;
  readonly onRuntimeSnapshotChange?: (
    snapshot: RuntimeSnapshot,
    state: { readonly stale: boolean; readonly pending: boolean },
  ) => void;
  readonly confirmRecoveryAbandon?: (
    title: string,
    body: string,
  ) => Promise<boolean>;
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
  onRecoveryPendingChange,
  onRuntimeSnapshotChange,
  confirmRecoveryAbandon = browserRecoveryConfirmation,
}: ChatWorkspaceScreenProps): React.JSX.Element {
  const runtime = useChatRuntime(corePort, pollIntervalMs);
  const commandIds = useMemo(() => new ChatWorkspaceController(), []);
  const [inspectorOpen, setInspectorOpen] = useState(true);
  const [inspectorWidth, setInspectorWidth] = useState(320);
  const chatLayoutRef = useRef<HTMLElement>(null);
  const previewInspectorWidth = useCallback((width: number) => {
    chatLayoutRef.current?.style.setProperty(
      "--aw-inspector-width",
      `${width}px`,
    );
  }, []);
  const [selectedTimelineId, setSelectedTimelineId] = useState<string | null>(null);
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
  const [workflowRequiresProject, setWorkflowRequiresProject] = useState<
    boolean | null
  >(nativeWorkflowPort === null ? false : null);
  const [workflowReadinessError, setWorkflowReadinessError] = useState<
    string | null
  >(null);
  const [confirmingRecoveryAbandon, setConfirmingRecoveryAbandon] =
    useState(false);
  const [stopRequested, setStopRequested] = useState(false);
  const handledNewChatRequest = useRef(0);
  const handledHistoryActionRequest = useRef(0);
  const wasActive = useRef(active);
  const snapshot = runtime.snapshot;
  const projectedRecoveryPending = snapshot?.chat.recoveryPending;
  const projectedChatId = snapshot?.chat.chatId;
  const timelineItems = useMemo(
    () => (snapshot === null ? [] : projectSemanticTimeline(runtime.events)),
    [snapshot, runtime.events],
  );
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

  // Load the saved-workflow library once so the composer can list and default
  // to the profile default workflow.
  useEffect(() => {
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
          currentId ?? library.defaultWorkflowId,
        );
      })
      .catch((failure: unknown) => {
        if (current) {
          setWorkflows([]);
          setDefaultWorkflowId(null);
          setSelectedWorkflowId(null);
          setWorkflowReadinessError(
            failure instanceof Error
              ? `Could not load the workflow library: ${failure.message}`
              : "Could not load the workflow library.",
          );
        }
      });
    return () => {
      current = false;
    };
  }, [libraryPort]);
  useEffect(() => {
    const reentered = active && !wasActive.current;
    wasActive.current = active;
    if (reentered) void runtime.resynchronize();
  }, [active, runtime.resynchronize]);
  useEffect(() => {
    if (!active) return;
    if (nativeWorkflowPort === null || selectedWorkflowId === null) {
      setWorkflowRequiresProject(false);
      setWorkflowReadinessError(null);
      return;
    }
    let current = true;
    setWorkflowRequiresProject(null);
    setWorkflowReadinessError(null);
    void nativeWorkflowPort
      .snapshot(selectedWorkflowId)
      .then(({ document, editable }) => {
        if (!current) return;
        if (!editable) {
          setWorkflowRequiresProject(null);
          setWorkflowReadinessError(
            "The selected workflow uses a read-only schema and cannot run.",
          );
          return;
        }
        setWorkflowRequiresProject(bindsProjectTools(document));
      })
      .catch(() => {
        if (current) {
          setWorkflowRequiresProject(null);
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
    setStopRequested(false);
  }, [projectedChatId]);
  useEffect(() => {
    if (!liveTurnRunning) setStopRequested(false);
  }, [liveTurnRunning]);
  useEffect(() => {
    if (
      newChatRequest <= handledNewChatRequest.current ||
      runtime.snapshot === null ||
      runtime.stale
    )
      return;
    handledNewChatRequest.current = newChatRequest;
    if (runtime.snapshot.chat.recoveryPending) return;
    setSelectedTimelineId(null);
    void runtime.dispatch(commandIds.createIntent("new_chat"));
  }, [commandIds, newChatRequest, runtime]);
  useEffect(() => {
    if (
      historyActionRequest === null ||
      historyActionRequest.requestId <= handledHistoryActionRequest.current ||
      runtime.snapshot === null ||
      runtime.stale
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
        <section className="route-loading" role="status">
          Connecting to the trusted core…
        </section>
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
  const visibleChat = liveTurnRunning
    ? { ...chat, phase: "running" as const }
    : chat;
  const control = (type: "cancel") => {
    setStopRequested(true);
    void runtime
      .dispatch(commandIds.createIntent(type, chat.chatId))
      .then((accepted) => {
        if (!accepted) setStopRequested(false);
      });
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
      ref={chatLayoutRef}
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
            <span className={`run-status ${visibleChat.phase}`}>
              <i />
              {label(visibleChat.phase)}
            </span>
            {controlsFor(visibleChat).includes("cancel") && (
              <button
                className="danger-action"
                disabled={chat.recoveryPending || stopRequested}
                title={
                  chat.recoveryPending
                    ? "Resume the interrupted command before stopping the Run"
                    : stopRequested
                      ? "Stopping the current response"
                    : "Stop the current response; completed workspace effects are not undone and the Chat remains open"
                }
                type="button"
                onClick={() => control("cancel")}
              >
                ■&nbsp; {stopRequested ? "Stopping…" : "Stop"}
              </button>
            )}
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
                Aworkit preserved the exact staged command. Resume replays that
                command; normal input, New Chat, and Stop remain locked.
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
        <ConversationTimeline
          active={active}
          items={timelineItems}
          selectedId={selectedTimelineId}
          actionsDisabled={runtime.pendingCommandIds.size > 0}
          onSelect={selectTimelineItem}
          onAction={cardAction}
        />
        <div className="chat-approval-control"><ApprovalModeSelect value={chat.approvalMode ?? "ask_for_approval"}
          disabled={runtime.stale || runtime.pendingCommandIds.size > 0 || liveTurnRunning || chat.recoveryPending}
          onChange={mode => void runtime.dispatch({ type: "approval_mode", commandId: commandIds.createIntent("approval_mode").commandId, targetId: chat.chatId, mode })} /></div>
        <ChatComposer
          key={chat.chatId + (defaultWorkflowId ?? "")}
          chat={chat}
          projects={snapshot.projects}
          stale={runtime.stale}
          pending={runtime.pendingCommandIds.size > 0}
          workflows={workflows}
          defaultWorkflowId={defaultWorkflowId}
          workflowRequiresProject={workflowRequiresProject}
          workflowReadinessError={workflowReadinessError}
          nextCommandId={() => commandIds.createIntent("enqueue").commandId}
          onWorkflowChange={setSelectedWorkflowId}
          onSubmit={runtime.dispatch}
        />
      </main>
      {inspectorOpen && (
        <PaneSplitter
          className="inspector-splitter"
          direction={-1}
          label="Resize Run details"
          max={420}
          min={280}
          value={inspectorWidth}
          onPreview={previewInspectorWidth}
          onChange={setInspectorWidth}
        />
      )}
      {inspectorOpen && (
        <RunDetailsInspector
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

function label(phase: string): string {
  if (phase === "waiting_input") return "Waiting for input";
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
