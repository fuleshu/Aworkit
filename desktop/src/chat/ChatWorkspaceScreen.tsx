import { useEffect, useMemo, useRef, useState } from "react";
import { PaneSplitter } from "../shell/PaneSplitter";
import type { ChatCorePort } from "./corePort";
import { ChatComposer } from "./ChatComposer";
import { ConversationTimeline } from "./ConversationTimeline";
import { controlsFor } from "./composer";
import { EvidenceInspector } from "./EvidenceInspector";
import { ChatWorkspaceController } from "./workspace";
import { useChatRuntime } from "./useChatRuntime";
import type { ChatIntent, TimelineItem } from "./types";

interface ChatWorkspaceScreenProps {
  readonly corePort?: ChatCorePort;
  readonly pollIntervalMs?: number;
  readonly newChatRequest?: number;
}

/** Complete projected Chat surface connected to the native trusted-core adapter. */
export function ChatWorkspaceScreen({
  corePort,
  pollIntervalMs,
  newChatRequest = 0,
}: ChatWorkspaceScreenProps): React.JSX.Element {
  const runtime = useChatRuntime(corePort, pollIntervalMs);
  const commandIds = useMemo(() => new ChatWorkspaceController(), []);
  const [inspectorOpen, setInspectorOpen] = useState(true);
  const [inspectorWidth, setInspectorWidth] = useState(320);
  const [selectedTimelineId, setSelectedTimelineId] = useState<string | null>(
    "tool.1",
  );
  const handledNewChatRequest = useRef(0);
  const snapshot = runtime.snapshot;
  useEffect(() => {
    if (
      newChatRequest <= handledNewChatRequest.current ||
      runtime.snapshot === null ||
      runtime.stale
    )
      return;
    handledNewChatRequest.current = newChatRequest;
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
  const control = (
    type: "pause" | "resume" | "cancel" | "retry" | "fork" | "continue",
  ) => {
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
  const selectedEvidence =
    snapshot.evidence.find((record) =>
      record.id.includes(selectedTimelineId?.split(".")[0] ?? ""),
    )?.id ??
    snapshot.evidence[0]?.id ??
    null;
  const secondaryControls = controlsFor(chat).filter((item) =>
    ["retry", "fork", "continue"].includes(item),
  );
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
              <span>
                {chat.workflowName} · {chat.branch} · {chat.runId}
              </span>
            </div>
          </div>
          <div className="run-actions">
            <span className={`run-status ${chat.phase}`}>
              <i />
              {label(chat.phase)}
            </span>
            {chat.phase === "running" && (
              <button
                title="Pause the active Run after its current safe boundary"
                type="button"
                onClick={() => control("pause")}
              >
                Ⅱ&nbsp; Pause
              </button>
            )}
            {chat.phase === "paused" && (
              <button
                title="Resume the paused Run"
                type="button"
                onClick={() => control("resume")}
              >
                ▶&nbsp; Resume
              </button>
            )}
            {controlsFor(chat).includes("cancel") && (
              <button
                className="danger-action"
                title="Cancel the Run; completed workspace effects are not undone"
                type="button"
                onClick={() => control("cancel")}
              >
                ■&nbsp; Cancel
              </button>
            )}
            {secondaryControls.length > 0 && (
              <details className="run-more">
                <summary title="Show additional Run actions">More</summary>
                <div>
                  {secondaryControls.map((item) => (
                    <button
                      key={item}
                      title={`${label(item)} this Run through the trusted core`}
                      type="button"
                      onClick={() =>
                        control(item as "retry" | "fork" | "continue")
                      }
                    >
                      {label(item)}
                    </button>
                  ))}
                </div>
              </details>
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
        {runtime.stale && (
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
        )}
        {runtime.error !== null && !runtime.stale && (
          <div className="command-banner" role="status">
            {runtime.error}
          </div>
        )}
        <ConversationTimeline
          items={snapshot.timeline}
          selectedId={selectedTimelineId}
          onSelect={setSelectedTimelineId}
          onAction={cardAction}
        />
        <ChatComposer
          chat={chat}
          stale={runtime.stale}
          pending={runtime.pendingCommandIds.size > 0}
          nextCommandId={() => commandIds.createIntent("enqueue").commandId}
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
  return phase
    .replaceAll("_", " ")
    .replace(/^./, (value) => value.toUpperCase());
}
