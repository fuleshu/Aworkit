import { useVirtualizer } from "@tanstack/react-virtual";
import { useEffect, useMemo, useRef } from "react";
import { ActorBubble } from "./ActorBubble";
import { ImageAttachments } from "./ImageAttachments";
import { toConversationCard } from "./conversation";
import {
  isModelCallSpan,
  ModelCallBlock,
  modelCallAssistantOutput,
} from "./ModelCallBlock";
import { prettyJson } from "./jsonPresentation";
import type { TimelineItem } from "./types";

interface ConversationTimelineProps {
  readonly items: readonly TimelineItem[];
  readonly selectedId: string | null;
  readonly actionsDisabled?: boolean;
  readonly onSelect: (id: string) => void;
  readonly onAction: (
    action: NonNullable<TimelineItem["action"]>,
    id: string,
  ) => void;
}

/** Virtualized semantic transcript; the complete ordered item list stays outside widget state. */
export function ConversationTimeline({
  items,
  selectedId,
  actionsDisabled = false,
  onSelect,
  onAction,
}: ConversationTimelineProps): React.JSX.Element {
  const scrollRef = useRef<HTMLDivElement>(null);
  const pinnedToEnd = useRef(true);
  const presentedItems = useMemo(() => presentTimelineItems(items), [items]);
  const presentedSelectedId = useMemo(
    () => presentedSelectionId(items, selectedId),
    [items, selectedId],
  );
  const layoutRevision = useMemo(
    () =>
      presentedItems
        .map(
          (item) =>
            `${item.id}\u0000${item.title.length}\u0000${item.body?.length ?? 0}\u0000${item.status ?? ""}\u0000${item.depth ?? 0}`,
        )
        .join("\u0001"),
    [presentedItems],
  );
  const virtualizer = useVirtualizer({
    count: presentedItems.length,
    getScrollElement: () => scrollRef.current,
    getItemKey: (index) => presentedItems[index]?.id ?? index,
    estimateSize: (index) => estimate(presentedItems[index]),
    overscan: 6,
  });
  useEffect(() => {
    let scrollFrame: number | null = null;
    const measurementFrame = window.requestAnimationFrame(() => {
      const scroll = scrollRef.current;
      if (scroll === null) return;
      for (const row of scroll.querySelectorAll<HTMLElement>(".virtual-row")) {
        const index = Number.parseInt(row.dataset.index ?? "", 10);
        if (Number.isInteger(index)) {
          virtualizer.resizeItem(index, row.getBoundingClientRect().height);
        }
      }
      if (pinnedToEnd.current && presentedItems.length > 0) {
        scrollFrame = window.requestAnimationFrame(() => {
          virtualizer.scrollToIndex(presentedItems.length - 1, { align: "end" });
        });
      }
    });
    return () => {
      window.cancelAnimationFrame(measurementFrame);
      if (scrollFrame !== null) window.cancelAnimationFrame(scrollFrame);
    };
  }, [presentedItems.length, layoutRevision, virtualizer]);
  useEffect(() => {
    const scroll = scrollRef.current;
    if (scroll === null) return;
    const pendingRows = new Set<HTMLElement>();
    let measurementFrame: number | null = null;
    let scrollFrame: number | null = null;
    const remeasureExpandedDetails = (event: Event) => {
      if (!(event.target instanceof HTMLDetailsElement)) return;
      const row = event.target.closest<HTMLElement>(".virtual-row");
      if (row === null || !scroll.contains(row)) return;
      pendingRows.add(row);
      if (measurementFrame !== null) return;
      // The toggle event follows the `open` state change, but its layout can
      // still be pending. Measure on the next frame, then preserve bottom pin
      // only after the virtual extent has incorporated the exact new height.
      measurementFrame = window.requestAnimationFrame(() => {
        measurementFrame = null;
        for (const pendingRow of pendingRows) {
          const index = Number.parseInt(pendingRow.dataset.index ?? "", 10);
          if (pendingRow.isConnected && Number.isInteger(index)) {
            // resizeItem bypasses the normal measurement cache and the
            // virtualizer's active-scroll deferral. Both expansion and
            // collapse therefore move every following row in this frame.
            virtualizer.resizeItem(
              index,
              pendingRow.getBoundingClientRect().height,
            );
          }
        }
        pendingRows.clear();
        if (pinnedToEnd.current && presentedItems.length > 0) {
          scrollFrame = window.requestAnimationFrame(() => {
            scrollFrame = null;
            virtualizer.scrollToIndex(presentedItems.length - 1, {
              align: "end",
            });
          });
        }
      });
    };
    scroll.addEventListener("toggle", remeasureExpandedDetails, true);
    return () => {
      scroll.removeEventListener("toggle", remeasureExpandedDetails, true);
      if (measurementFrame !== null)
        window.cancelAnimationFrame(measurementFrame);
      if (scrollFrame !== null) window.cancelAnimationFrame(scrollFrame);
    };
  }, [presentedItems.length, virtualizer]);
  return (
    <div
      aria-live="polite"
      aria-relevant="additions text"
      className="timeline-scroll"
      ref={scrollRef}
      role="log"
      onScroll={(event) => {
        const target = event.currentTarget;
        pinnedToEnd.current =
          target.scrollHeight - target.scrollTop - target.clientHeight < 48;
      }}
    >
      {items.length === 0 ? (
        <p className="empty-state timeline-empty">
          No messages yet. Configure a provider in Settings, then send the
          first message.
        </p>
      ) : (
        <div className="timeline-date">TODAY</div>
      )}
      <div
        className="virtual-timeline"
        style={{ height: virtualizer.getTotalSize() }}
      >
        {virtualizer.getVirtualItems().map((row) => {
          const item = presentedItems[row.index];
          const card = toConversationCard(item);
          return (
            <div
              className="virtual-row"
              data-index={row.index}
              key={item.id}
              ref={virtualizer.measureElement}
              style={{
                transform: `translateY(${row.start}px)`,
                paddingInlineStart: `${Math.min(item.depth ?? 0, 5) * 14}px`,
              }}
            >
              <TimelineCard
                card={card}
                item={item}
                selected={presentedSelectedId === item.id}
                actionsDisabled={actionsDisabled}
                onSelect={onSelect}
                onAction={onAction}
              />
            </div>
          );
        })}
      </div>
    </div>
  );
}

export function TimelineCard({
  card,
  item,
  selected,
  actionsDisabled = false,
  onSelect,
  onAction,
}: {
  readonly card: ReturnType<typeof toConversationCard>;
  readonly item: TimelineItem;
  readonly selected: boolean;
  readonly actionsDisabled?: boolean;
  readonly onSelect: (id: string) => void;
  readonly onAction: ConversationTimelineProps["onAction"];
}): React.JSX.Element {
  if (isModelCallSpan(item))
    return (
      <ModelCallBlock item={item} selected={selected} onSelect={onSelect} />
    );
  if (item.kind === "message")
    return item.title === "You" ? (
      <article
        className={`message-row from-user ${selected ? "selected" : ""}`}
        aria-label={`${item.title} message`}
      >
        <div className="message-body user-speech-bubble">
          <ImageAttachments images={item.attachments ?? []} />
          <p>{item.body}</p>
          <div className="message-byline">
            <small>{item.title} · {item.createdAt}</small>
            <button
              title="Show Run details for this message"
              type="button"
              onClick={() => onSelect(item.id)}
            >
              Details
            </button>
          </div>
        </div>
      </article>
    ) : (
      <ActorBubble
        actor="model"
        ariaLabel={`${item.title} message`}
        body={item.body ?? ""}
        createdAt={item.createdAt}
        onSelect={() => onSelect(item.id)}
        selected={selected}
        variant="speech"
      />
    );
  if (item.kind === "plan") {
    const busy = isBusy(item.status);
    const progress =
      typeof item.metadata === "object" &&
      item.metadata !== null &&
      "completed" in item.metadata
        ? Number(item.metadata.completed)
        : 0;
    const total =
      typeof item.metadata === "object" &&
      item.metadata !== null &&
      "total" in item.metadata
        ? Number(item.metadata.total)
        : 4;
    return (
      <article
        aria-busy={busy || undefined}
        className={`activity-card plan-card ${selected ? "selected" : ""}`}
      >
        <button
          className="activity-heading activity-select-heading"
          title={`Show Run details for ${item.title}`}
          type="button"
          onClick={() => onSelect(item.id)}
        >
          <span className="activity-icon">{busy ? "" : statusIcon(item)}</span>
          <strong>{item.title}</strong>
          <span>{item.status}</span>
        </button>
        <div
          aria-label="Plan progress"
          aria-valuemax={total}
          aria-valuemin={0}
          aria-valuenow={progress}
          className="progress-track"
          role="progressbar"
        >
          <span
            style={{
              width: `${Math.min(100, (progress / Math.max(1, total)) * 100)}%`,
            }}
          />
        </div>
        <ol>
          {item.body?.split("\n").map((line, index) => (
            <li className={index < progress ? "done" : ""} key={line}>
              {line}
            </li>
          ))}
        </ol>
      </article>
    );
  }
  if (isPlanCall(item))
    return (
      <article className={`activity-card plan-call-card ${selected ? "selected" : ""}`} aria-label={`Plan: ${item.title}`}>
        <button
          className="activity-heading activity-select-heading"
          title={`Show Run details for ${item.title}`}
          type="button"
          onClick={() => onSelect(item.id)}
        >
          <span className="activity-icon">◇</span>
          <strong>{item.title}</strong>
          <span>{item.status}</span>
        </button>
        <p className="plan-call-body">{item.body}</p>
      </article>
    );
  if (isWebResult(item))
    return (
      <article className={`activity-card web-result-card ${selected ? "selected" : ""}`} aria-label={`${webTitle(item)}: ${item.title}`}>
        <button className="activity-main" type="button" title={`Show Run details for ${item.title}`} onClick={() => onSelect(item.id)}>
          <span className="activity-icon">⌕</span>
          <span>
            <strong>{webTitle(item)}</strong>
            <code>{item.body}</code>
          </span>
          <span className={`status ${item.status ?? ""}`}>{item.status ?? "result"}</span>
        </button>
        {card.inspectable && (
          <details className="activity-raw">
            <summary>Inspect source record</summary>
            <pre>{safeJson(item.raw ?? item.metadata ?? item)}</pre>
          </details>
        )}
      </article>
    );
  if (item.kind === "todo")
    return (
      <article className={`activity-card todo-card ${selected ? "selected" : ""}`} aria-label={`Task list: ${item.title}`}>
        <button
          className="activity-heading activity-select-heading"
          title={`Show Run details for ${item.title}`}
          type="button"
          onClick={() => onSelect(item.id)}
        >
          <span className="activity-icon">☑</span>
          <strong>{item.title}</strong>
          <span>{todoCount(item)}</span>
        </button>
        <ul className="todo-list">
          {todosOf(item).map((todo, index) => (
            <li
              className={todo.done ? "done" : todo.active ? "active" : ""}
              key={`${index}-${todo.content}`}
            >
              {todo.content}
            </li>
          ))}
        </ul>
      </article>
    );
  if (item.kind === "subagent") {
    const speech = subagentFinalText(item);
    return (
      <ActorBubble
        actor="subagent"
        ariaLabel={`Subagent: ${item.title}`}
        body={speech ?? item.body ?? "Delegated work is in progress."}
        busy={isBusy(item.status)}
        createdAt={speech === undefined ? undefined : item.createdAt}
        onSelect={() => onSelect(item.id)}
        selected={selected}
        status={item.status}
        title={speech === undefined ? item.title : undefined}
        variant={speech === undefined ? "thinking" : "speech"}
      >
        {speech === undefined && <ActivityData collapsed item={item} />}
        {card.inspectable && (
          <details className="activity-raw">
            <summary>Inspect source record</summary>
            <pre>{safeJson(item.raw ?? item.metadata ?? item)}</pre>
          </details>
        )}
      </ActorBubble>
    );
  }
  if (item.kind === "thinking")
    return (
      <ActorBubble
        actor={item.actor ?? "model"}
        ariaLabel={`${card.label}: ${item.title}`}
        body={card.content}
        busy={isBusy(item.status)}
        onSelect={() => onSelect(item.id)}
        reasoningLabel={card.reasoningLabel}
        selected={selected}
        status={item.status}
        title={item.title}
        variant="thinking"
      >
        <ActivityData collapsed item={item} />
        {card.inspectable && (
          <details className="activity-raw">
            <summary>Inspect source record</summary>
            <pre>{safeJson(item.raw ?? item.metadata ?? item)}</pre>
          </details>
        )}
      </ActorBubble>
    );
  if (metadataOf(item).live === true)
    return (
      <article
        aria-busy={isBusy(item.status) || undefined}
        className={`activity-card live-activity-card ${selected ? "selected" : ""}`}
        aria-label={`${card.label}: ${item.title}`}
      >
        <div className="activity-main">
          <span className="activity-icon">
            {isBusy(item.status) ? "" : icon(item.kind, item.status)}
          </span>
          <span>
            <strong>{item.title}</strong>
            {card.reasoningLabel !== undefined && (
              <small className="reasoning-label">{card.reasoningLabel}</small>
            )}
            <code>{card.content}</code>
            <ActivityData item={item} />
          </span>
          <span className={`status ${item.status ?? ""}`}>
            {item.status ?? card.label}
          </span>
        </div>
        <button
          className="activity-details-button"
          title={`Show Run details for ${item.title}`}
          type="button"
          onClick={() => onSelect(item.id)}
        >
          Details
        </button>
      </article>
    );
  return (
    <article
      aria-busy={isBusy(item.status) || undefined}
      className={`activity-card ${item.kind === "approval" ? "approval-card" : ""} ${
        selected ? "selected" : ""
      }`}
      aria-label={`${card.label}: ${item.title}`}
    >
      <button
        className="activity-main"
        type="button"
        title={`Show Run details for ${item.title}`}
        onClick={() => onSelect(item.id)}
      >
        <span className="activity-icon">
          {isBusy(item.status) ? "" : icon(item.kind, item.status)}
        </span>
        <span>
          <strong>{item.title}</strong>
          {card.reasoningLabel !== undefined && (
            <small className="reasoning-label">{card.reasoningLabel}</small>
          )}
          <code>{card.content}</code>
        </span>
        <span className={`status ${item.status ?? ""}`}>
          {item.status ?? card.label}
        </span>
      </button>
      <ActivityData item={item} />
      {item.kind === "approval" && item.action === "approve" ? (
        <div className="activity-actions">
          <button
            disabled={actionsDisabled}
            type="button"
            title={
              actionsDisabled
                ? "Wait for the current Chat command to settle"
                : "Approve this requested action"
            }
            onClick={() => onAction("approve", item.id)}
          >
            Approve
          </button>
          <button
            disabled={actionsDisabled}
            type="button"
            title={
              actionsDisabled
                ? "Wait for the current Chat command to settle"
                : "Reject this requested action"
            }
            onClick={() => onAction("reject", item.id)}
          >
            Reject
          </button>
        </div>
      ) : card.action !== undefined ? (
        <button
          type="button"
          title={`${card.action} this activity`}
          onClick={() => onAction(card.action!, item.id)}
        >
          {card.action}
        </button>
      ) : null}
      {card.inspectable && (
        <details className="activity-raw">
          <summary>Inspect source record</summary>
          <pre>{safeJson(item.raw ?? item.metadata ?? item)}</pre>
        </details>
      )}
    </article>
  );
}

function estimate(item: TimelineItem | undefined): number {
  if (item?.kind === "plan" || item?.kind === "todo") return 168;
  if (item !== undefined && isModelCallSpan(item)) return 220;
  if (item?.kind === "message") return 92;
  if (item?.kind === "subagent") return 96;
  return item !== undefined && toConversationCard(item).inspectable ? 132 : 92;
}

/** Removes only an assistant message already rendered from its exact stream. */
export function presentTimelineItems(
  items: readonly TimelineItem[],
): readonly TimelineItem[] {
  return items.filter((item, index) => {
    if (item.kind !== "message" || item.title === "You") return true;
    return mirroredModelCall(items, index, item.body ?? "") === undefined;
  });
}

function presentedSelectionId(
  items: readonly TimelineItem[],
  selectedId: string | null,
): string | null {
  if (selectedId === null) return null;
  const index = items.findIndex(({ id }) => id === selectedId);
  if (index < 0) return selectedId;
  const item = items[index];
  if (item.kind !== "message" || item.title === "You") return selectedId;
  return mirroredModelCall(items, index, item.body ?? "")?.id ?? selectedId;
}

function mirroredModelCall(
  items: readonly TimelineItem[],
  assistantIndex: number,
  assistantBody: string,
): TimelineItem | undefined {
  const expected = assistantBody.trim();
  if (expected.length === 0) return undefined;
  for (let index = assistantIndex - 1; index >= 0; index -= 1) {
    const candidate = items[index];
    if (candidate.kind === "message" && candidate.title === "You") break;
    if (
      isModelCallSpan(candidate) &&
      modelCallAssistantOutput(candidate).trim() === expected
    ) {
      return candidate;
    }
  }
  return undefined;
}

function safeJson(value: unknown): string {
  return prettyJson(value, "Source record is not serializable.");
}

function ActivityData({
  collapsed = false,
  item,
}: {
  readonly collapsed?: boolean;
  readonly item: TimelineItem;
}): React.JSX.Element | null {
  const metadata = metadataOf(item);
  const nodeType = typeof metadata.nodeType === "string" ? metadata.nodeType : undefined;
  // Pass-through control nodes retain exact data in their source record, but
  // repeating the same value as both input and output obscures the useful flow.
  if (nodeType === "output" || nodeType === "wait" || nodeType === "completion")
    return null;
  const hasInput =
    item.input !== undefined ||
    metadata.hasInput === true ||
    (metadata.live === true && Object.hasOwn(metadata, "input"));
  const hasOutput =
    item.output !== undefined ||
    metadata.hasOutput === true ||
    (metadata.live === true && Object.hasOwn(metadata, "output"));
  if (!hasInput && !hasOutput) return null;
  const input = item.input !== undefined ? item.input : metadata.input;
  const output = item.output !== undefined ? item.output : metadata.output;
  const data = (
    <dl className="activity-data" aria-label={`${item.title} data flow`}>
      {hasInput && (
        <div>
          <dt>Input</dt>
          <dd><pre>{formatActivityData(input)}</pre></dd>
        </div>
      )}
      {hasOutput && (
        <div>
          <dt>Output</dt>
          <dd><pre>{formatActivityData(output)}</pre></dd>
        </div>
      )}
    </dl>
  );
  return collapsed ? (
    <details className="bubble-data-details">
      <summary>Input &amp; output</summary>
      {data}
    </details>
  ) : (
    data
  );
}

function formatActivityData(value: unknown): string {
  return typeof value === "string" ? value : safeJson(value);
}
function icon(kind: TimelineItem["kind"], status?: string): string {
  return kind === "thinking"
    ? status === "completed"
      ? "✓"
      : ""
    : kind === "tool"
    ? ">_"
    : kind === "approval"
      ? "!"
      : kind === "error"
        ? "×"
        : kind === "verification"
          ? "✓"
          : "◇";
}

function statusIcon(item: TimelineItem): string {
  if (item.status === "failed") return "×";
  if (item.status === "completed") return "✓";
  return icon(item.kind, item.status);
}

function isBusy(status: string | undefined): boolean {
  return status === "running" || status === "started" || status === "queued";
}

function metadataOf(item: TimelineItem): Record<string, unknown> {
  const value = item.metadata;
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}

/** A plan card is the output of a model_call node committed as node.completed. */
function isPlanCall(item: TimelineItem): boolean {
  return item.kind === "model" && metadataOf(item).nodeType === "model_call";
}

/** A web-result card is a tool call settled by the built-in web capabilities. */
function isWebResult(item: TimelineItem): boolean {
  if (item.kind !== "tool") return false;
  const capabilityId = metadataOf(item).capabilityId;
  return (
    capabilityId === "tool.web_search" ||
    capabilityId === "tool.web_fetch" ||
    capabilityId === "tool.web_extract"
  );
}

function webTitle(item: TimelineItem): string {
  const capabilityId = metadataOf(item).capabilityId;
  if (capabilityId === "tool.web_extract") return "Web extract";
  return capabilityId === "tool.web_fetch" ? "Web fetch" : "Web search";
}

function todosOf(item: TimelineItem): readonly {
  readonly content: string;
  readonly done: boolean;
  readonly active: boolean;
}[] {
  const todos = metadataOf(item).todos;
  if (!Array.isArray(todos)) return [];
  return todos.map((todo) => {
    const record =
      typeof todo === "object" && todo !== null && !Array.isArray(todo)
        ? (todo as Record<string, unknown>)
        : {};
    const content =
      typeof record.content === "string" ? record.content : String(record.content ?? "");
    const status = typeof record.status === "string" ? record.status : "";
    return {
      content,
      done: status === "completed" || status === "done",
      active: status === "in_progress",
    };
  });
}

function todoCount(item: TimelineItem): string {
  const todos = todosOf(item);
  const done = todos.filter((todo) => todo.done).length;
  return `${done}/${todos.length}`;
}

function subagentFinalText(item: TimelineItem): string | undefined {
  for (const value of [item.output, metadataOf(item).output]) {
    if (typeof value !== "object" || value === null || Array.isArray(value))
      continue;
    const finalText = (value as Record<string, unknown>).finalText;
    if (typeof finalText === "string" && finalText.trim().length > 0)
      return finalText;
  }
  return undefined;
}
