import { useEffect, useMemo, useRef, useState } from "react";
import { PaneSplitter } from "../shell/PaneSplitter";
import { deriveActivityCards, mergeTimeline } from "./activityProjection";
import type { ChatCorePort } from "./corePort";
import {
  ChatComposer,
  type WorkflowOption,
} from "./ChatComposer";
import { ConversationTimeline } from "./ConversationTimeline";
import { controlsFor } from "./composer";
import { EvidenceInspector } from "./EvidenceInspector";
import { ChatWorkspaceController } from "./workspace";
import { useChatRuntime } from "./useChatRuntime";
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
  readonly active?: boolean;
  readonly workflowPort?: Pick<WorkflowCorePort, "snapshot">;
  readonly libraryPort?: WorkflowLibraryPort;
  readonly onRecoveryPendingChange?: (pending: boolean) => void;
  readonly confirmRecoveryAbandon?: (
    title: string,
    body: string,
  ) => Promise<boolean>;
}

/** Complete projected Chat surface connected to the native trusted-core adapter. */
export function ChatWorkspaceScreen({
  corePort,
  pollIntervalMs,
  newChatRequest = 0,
  active = true,
  workflowPort,
  libraryPort,
  onRecoveryPendingChange,
  confirmRecoveryAbandon = browserRecoveryConfirmation,
}: ChatWorkspaceScreenProps): React.JSX.Element {
  const runtime = useChatRuntime(corePort, pollIntervalMs);
  const commandIds = useMemo(() => new ChatWorkspaceController(), []);
  const [inspectorOpen, setInspectorOpen] = useState(true);
  const [inspectorWidth, setInspectorWidth] = useState(320);
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
  const handledNewChatRequest = useRef(0);
  const wasActive = useRef(active);
  const snapshot = runtime.snapshot;
  const projectedRecoveryPending = snapshot?.chat.recoveryPending;
  const timelineItems = useMemo(
    () =>
      snapshot === null
        ? []
        : mergeTimeline(
            snapshot.timeline,
            deriveActivityCards(runtime.events),
          ),
    [snapshot, runtime.events],
  );

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
      .catch(() => {
        if (current) {
          setWorkflows([{ id: "workflow.simple-chat", name: "Simple Chat" }]);
          setDefaultWorkflowId("workflow.simple-chat");
          setSelectedWorkflowId((currentId) =>
            currentId ?? "workflow.simple-chat",
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
  if (runtime.loading && snapshot === null)
    return (
      <section className="route-loading" role="status">
        Connecting to the trusted core…
      </section>
    );
  if (snapshot === null)
    return (
      <section className="route-error" role="alert">
        <h2>Chat projection unavailable</h2>
        <p>{runtime.error}</p>
        <button
          type="button"
          title="Retry the trusted-core projection query"
          onClick={() => void runtime.resynchronize()}
        >
          Retry
        </button>
      </section>
    );
  const chat = snapshot.chat;
  const control = (type: "cancel") => {
    void runtime.dispatch(commandIds.createIntent(type));
  };
  const cardAction = (
    action: NonNullable<TimelineItem["action"]>,
    targetId: string,
  ) => {
    const intent = timelineActionIntent(
      action,
      targetId,
      commandIds.createIntent(
        action === "approve" || action === "reject" ? "approval" : action,
      ).commandId,
    );
    void runtime.dispatch(intent);
  };
  const expectedEvidenceId =
    selectedTimelineId === null ? null : `evidence.${selectedTimelineId}`;
  const selectedEvidence =
    (expectedEvidenceId !== null &&
    snapshot.evidence.some(({ id }) => id === expectedEvidenceId)
      ? expectedEvidenceId
      : null) ??
    snapshot.evidence[0]?.id ??
    null;
  const chatContext = [
    chat.workflowName,
    chat.branch,
    chat.runId === "run.draft" ? null : chat.runId,
  ].filter((item): item is string => item !== null);
  return (
    <section
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
            <span className={`run-status ${chat.phase}`}>
              <i />
              {label(chat.phase)}
            </span>
            {controlsFor(chat).includes("cancel") && (
              <button
                className="danger-action"
                disabled={chat.recoveryPending}
                title={
                  chat.recoveryPending
                    ? "Resume the interrupted command before cancelling the Run"
                    : "Cancel the Run; completed workspace effects are not undone"
                }
                type="button"
                onClick={() => control("cancel")}
              >
                ■&nbsp; Cancel
              </button>
            )}
            <button
              aria-pressed={inspectorOpen}
              title="Show or hide the evidence inspector"
              type="button"
              onClick={() => setInspectorOpen((open) => !open)}
            >
              Evidence
            </button>
          </div>
        </header>
        {chat.recoveryPending ? (
          <div className="recovery-banner" role="status">
            <div>
              <strong>Interrupted command requires an explicit decision.</strong>
              <p>
                Aworkit preserved the exact staged command. Resume replays that
                command; normal input, New Chat, and Cancel remain locked.
              </p>
              {runtime.error !== null && (
                <p className="field-error" role="alert">
                  Recovery command failed: {runtime.error} You can retry Resume
                  or explicitly abandon the staged command as outcome-uncertain.
                </p>
              )}
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
        ) : runtime.stale ? (
          <div className="stale-banner" role="status">
            <strong>Projection disconnected.</strong> Last contiguous state is
            frozen; changes are disabled.
            <button
              title="Request a fresh trusted-core snapshot"
              type="button"
              onClick={() => void runtime.resynchronize()}
            >
              Resync
            </button>
          </div>
        ) : runtime.error !== null ? (
          <div className="command-banner" role="status">
            {runtime.error}
          </div>
        ) : null}
        <ConversationTimeline
          items={timelineItems}
          selectedId={selectedTimelineId}
          onSelect={setSelectedTimelineId}
          onAction={cardAction}
        />
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
          label="Resize evidence inspector"
          max={420}
          min={280}
          value={inspectorWidth}
          onChange={setInspectorWidth}
        />
      )}
      {inspectorOpen && (
        <EvidenceInspector
          records={snapshot.evidence}
          selectedId={selectedEvidence}
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
): ChatIntent {
  return action === "approve" || action === "reject"
    ? {
        type: "approval",
        commandId,
        targetId,
        approved: action === "approve",
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
