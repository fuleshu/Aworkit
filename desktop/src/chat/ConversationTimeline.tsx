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
  const virtualizer = useVirtualizer({
    count: items.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: (index) => estimate(items[index]),
    overscan: 6,
  });
  useEffect(() => {
    if (pinnedToEnd.current && items.length > 0)
      virtualizer.scrollToIndex(items.length - 1, { align: "end" });
  }, [items.length, virtualizer]);
  return (
    <div
      aria-live="polite"
      aria-relevant="additions"
      className="timeline-scroll"
      ref={scrollRef}
      role="log"
      onScroll={(event) => {
        const target = event.currentTarget;
        pinnedToEnd.current =
          target.scrollHeight - target.scrollTop - target.clientHeight < 48;
      }}
    >
      <div className="timeline-date">TODAY · 12:41</div>
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
      <article className="activity-card plan-card">
        <div className="activity-heading">
          <span className="activity-icon">✓</span>
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
  return (
    <article
      className={`activity-card ${selected ? "selected" : ""}`}
      aria-label={`${card.label}: ${item.title}`}
    >
      <button
        className="activity-main"
        type="button"
        title={`Inspect ${item.title} evidence`}
        onClick={() => onSelect(item.id)}
      >
        <span className="activity-icon">{icon(item.kind)}</span>
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
  if (item?.kind === "plan") return 168;
  if (item?.kind === "message") return 92;
  return item !== undefined && toConversationCard(item).inspectable ? 132 : 92;
}

function safeJson(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return "Source record is not serializable.";
  }
}
function icon(kind: TimelineItem["kind"]): string {
  return kind === "tool"
    ? ">_"
    : kind === "approval"
      ? "!"
      : kind === "error"
        ? "×"
        : kind === "verification"
          ? "✓"
          : "◇";
}
