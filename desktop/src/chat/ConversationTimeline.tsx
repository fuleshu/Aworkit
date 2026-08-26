import { useVirtualizer } from "@tanstack/react-virtual";
import { useEffect, useRef } from "react";
import { toConversationCard } from "./conversation";
import type { TimelineItem } from "./types";

interface ConversationTimelineProps {
  readonly items: readonly TimelineItem[];
  readonly selectedId: string | null;
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
  onSelect,
  onAction,
}: ConversationTimelineProps): React.JSX.Element {
  const scrollRef = useRef<HTMLDivElement>(null);
  const pinnedToEnd = useRef(true);
  const layoutRevision = items
    .map(
      (item) =>
        `${item.id}\u0000${item.title.length}\u0000${item.body?.length ?? 0}\u0000${item.status ?? ""}`,
    )
    .join("\u0001");
  const virtualizer = useVirtualizer({
    count: items.length,
    getScrollElement: () => scrollRef.current,
    getItemKey: (index) => items[index]?.id ?? index,
    estimateSize: (index) => estimate(items[index]),
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
      if (pinnedToEnd.current && items.length > 0) {
        scrollFrame = window.requestAnimationFrame(() => {
          virtualizer.scrollToIndex(items.length - 1, { align: "end" });
        });
      }
    });
    return () => {
      window.cancelAnimationFrame(measurementFrame);
      if (scrollFrame !== null) window.cancelAnimationFrame(scrollFrame);
    };
  }, [items.length, layoutRevision, virtualizer]);
  useEffect(() => {
    const scroll = scrollRef.current;
    if (scroll === null) return;
    const pendingRows = new Set<HTMLElement>();
    let measurementFrame: number | null = null;
    let scrollFrame: number | null = null;
    const remeasureExpandedEvidence = (event: Event) => {
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
        if (pinnedToEnd.current && items.length > 0) {
          scrollFrame = window.requestAnimationFrame(() => {
            scrollFrame = null;
            virtualizer.scrollToIndex(items.length - 1, { align: "end" });
          });
        }
      });
    };
    scroll.addEventListener("toggle", remeasureExpandedEvidence, true);
    return () => {
      scroll.removeEventListener("toggle", remeasureExpandedEvidence, true);
      if (measurementFrame !== null)
        window.cancelAnimationFrame(measurementFrame);
      if (scrollFrame !== null) window.cancelAnimationFrame(scrollFrame);
    };
  }, [items.length, virtualizer]);
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
          const item = items[row.index];
          const card = toConversationCard(item);
          return (
            <div
              className="virtual-row"
              data-index={row.index}
              key={item.id}
              ref={virtualizer.measureElement}
              style={{ transform: `translateY(${row.start}px)` }}
            >
              <TimelineCard
                card={card}
                item={item}
                selected={selectedId === item.id}
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
  onSelect,
  onAction,
}: {
  readonly card: ReturnType<typeof toConversationCard>;
  readonly item: TimelineItem;
  readonly selected: boolean;
  readonly onSelect: (id: string) => void;
  readonly onAction: ConversationTimelineProps["onAction"];
}): React.JSX.Element {
  if (item.kind === "message")
    return (
      <article
        className={`message-row ${item.title === "You" ? "from-user" : "from-assistant"}`}
        aria-label={`${item.title} message`}
      >
        <div className="message-body">
          <p>{item.body}</p>
          <small>
            {item.title} · {item.createdAt}
          </small>
        </div>
      </article>
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
        className="activity-card plan-card"
      >
        <div className="activity-heading">
          <span className="activity-icon">{busy ? "" : statusIcon(item)}</span>
          <strong>{item.title}</strong>
          <span>{item.status}</span>
        </div>
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
      <article className="activity-card plan-call-card" aria-label={`Plan: ${item.title}`}>
        <div className="activity-heading">
          <span className="activity-icon">◇</span>
          <strong>{item.title}</strong>
          <span>{item.status}</span>
        </div>
        <p className="plan-call-body">{item.body}</p>
      </article>
    );
  if (isWebResult(item))
    return (
      <article className={`activity-card web-result-card ${selected ? "selected" : ""}`} aria-label={`${webTitle(item)}: ${item.title}`}>
        <button className="activity-main" type="button" title={`Inspect ${item.title} evidence`} onClick={() => onSelect(item.id)}>
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
      <article className="activity-card todo-card" aria-label={`Task list: ${item.title}`}>
        <div className="activity-heading">
          <span className="activity-icon">☑</span>
          <strong>{item.title}</strong>
          <span>{todoCount(item)}</span>
        </div>
        <ul className="todo-list">
          {todosOf(item).map((todo, index) => (
            <li className={todo.done ? "done" : ""} key={`${index}-${todo.content}`}>
              {todo.content}
            </li>
          ))}
        </ul>
      </article>
    );
  if (item.kind === "subagent")
    return (
      <article className={`activity-card subagent-card ${selected ? "selected" : ""}`} aria-label={`Subagent: ${item.title}`}>
        <details>
          <summary className="activity-heading">
            <span className="activity-icon">≋</span>
            <strong>{item.title}</strong>
            <span className={`status ${item.status ?? ""}`}>{item.status ?? "run"}</span>
          </summary>
          <p className="subagent-body">{item.body}</p>
          {card.inspectable && (
            <details className="activity-raw">
              <summary>Inspect source record</summary>
              <pre>{safeJson(item.raw ?? item.metadata ?? item)}</pre>
            </details>
          )}
        </details>
      </article>
    );
  if (metadataOf(item).live === true)
    return (
      <article
        aria-busy={isBusy(item.status) || undefined}
        className={`activity-card live-activity-card ${item.kind === "thinking" ? "thinking-card" : ""}`}
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
      </article>
    );
  return (
    <article
      aria-busy={isBusy(item.status) || undefined}
      className={`activity-card ${item.kind === "thinking" ? "thinking-card" : ""} ${selected ? "selected" : ""}`}
      aria-label={`${card.label}: ${item.title}`}
    >
      <button
        className="activity-main"
        type="button"
        title={`Inspect ${item.title} evidence`}
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
      {item.kind === "approval" ? (
        <div className="activity-actions">
          <button
            type="button"
            title="Approve this requested action"
            onClick={() => onAction("approve", item.id)}
          >
            Approve
          </button>
          <button
            type="button"
            title="Reject this requested action"
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
  if (item?.kind === "message") return 92;
  if (item?.kind === "subagent") return 96;
  return item !== undefined && toConversationCard(item).inspectable ? 132 : 92;
}

function safeJson(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return "Source record is not serializable.";
  }
}

function ActivityData({ item }: { readonly item: TimelineItem }): React.JSX.Element | null {
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
  return (
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
    capabilityId === "tool.web_search" || capabilityId === "tool.web_fetch"
  );
}

function webTitle(item: TimelineItem): string {
  return metadataOf(item).capabilityId === "tool.web_fetch"
    ? "Web fetch"
    : "Web search";
}

function todosOf(item: TimelineItem): readonly { readonly content: string; readonly done: boolean }[] {
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
    return { content, done: status === "completed" || status === "done" };
  });
}

function todoCount(item: TimelineItem): string {
  const todos = todosOf(item);
  const done = todos.filter((todo) => todo.done).length;
  return `${done}/${todos.length}`;
}
