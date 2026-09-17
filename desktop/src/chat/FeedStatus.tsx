import type { TimelineItem } from "./types";

/** Compact terminal context; full parent inputs, output and children stay inspectable. */
export function FeedStatus({ item, selected, onSelect }: {
  readonly item: TimelineItem;
  readonly selected: boolean;
  readonly onSelect: (id: string) => void;
}) {
  return <article className={`feed-status ${selected ? "selected" : ""}`} aria-label={`${item.title}: ${item.status}`}>
    <button type="button" title={`Show Run details for ${item.title}`} onClick={() => onSelect(item.id)}>
      <strong>{item.title}</strong><span className={`status ${item.status ?? ""}`}>{item.status}</span>
      {item.body && <span className="feed-status-body">{item.body}</span>}
    </button>
  </article>;
}
