import { ConversationTimeline } from "./ConversationTimeline";
import { MarkdownContent } from "./MarkdownContent";
import type { SubagentCatalogEntry } from "./subagentCatalog";
import { subagentStatusLabel } from "./SubagentTabs";
import type { TimelineItem } from "./types";

interface SubagentConversationProps {
  readonly entry: SubagentCatalogEntry;
  readonly items: readonly TimelineItem[];
  readonly selectedId: string | null;
  readonly onSelect: (id: string) => void;
  readonly hasOlder: boolean;
  readonly olderLoading: boolean;
  readonly olderError: string | null;
  /** The child scope has not finished reading its own evidence yet. */
  readonly loading: boolean;
  readonly onLoadOlder: () => Promise<void>;
  readonly active: boolean;
}

/**
 * One delegated child rendered like the main conversation but read-only: the
 * child's own bounded evidence with a header that carries its lineage, status
 * and assigned task. There is no composer here — a child never accepts user
 * steering, and this view never creates a second Run or history.
 */
export function SubagentConversation({
  entry,
  items,
  selectedId,
  onSelect,
  hasOlder,
  olderLoading,
  olderError,
  loading,
  onLoadOlder,
  active,
}: SubagentConversationProps): React.JSX.Element {
  const task = entry.task.trim();
  return (
    <div className="subagent-conversation">
      <header className="subagent-child-header">
        <div>
          <p className="eyebrow">
            {entry.kind === "fork" ? "FORKED SUBAGENT" : "SUBAGENT"}
            {entry.depth > 1 ? ` · DEPTH ${entry.depth}` : ""}
          </p>
          <h2 title={task.length > 0 ? task : entry.childId}>
            {task.length > 0 ? task : "Delegated task"}
          </h2>
          <p className="subagent-lineage">
            Child <code>{entry.childId}</code>
            {entry.nodeId !== undefined && entry.nodeId.length > 0 && (
              <> · delegated by <code>{entry.nodeId}</code></>
            )}
            {` · ${entry.modelTurns} model turn(s), ${entry.toolCalls} tool call(s)`}
            {entry.inputTokens + entry.outputTokens > 0 &&
              ` · ${entry.inputTokens + entry.outputTokens} token(s)`}
          </p>
        </div>
        <span className={`subagent-status ${entry.status}`} role="status">
          {subagentStatusLabel(entry.status)}
        </span>
      </header>
      {entry.contextText.trim().length > 0 && (
        <details className="subagent-context">
          <summary>Assigned context</summary>
          <pre>{entry.contextText}</pre>
        </details>
      )}
      <p className="subagent-readonly" role="note">
        Read-only. This tab shows the subagent's own evidence from this Run's
        history. Steering and approval stay with the delegating Agent.
      </p>
      {items.length === 0 ? (
        <div className="subagent-empty" role="status">
          {loading ? (
            "Loading this subagent's activity…"
          ) : olderError !== null ? (
            <>
              <p role="alert">
                This subagent's activity could not be read: {olderError}
              </p>
              <button
                type="button"
                title="Read this subagent's activity again"
                onClick={() => void onLoadOlder()}
              >
                Retry
              </button>
            </>
          ) : entry.finalText.trim().length > 0 ? (
            // The child settled with an answer but committed no rendered
            // activity of its own, so its answer still gets normal formatting.
            <MarkdownContent className="bubble-markdown">
              {entry.finalText}
            </MarkdownContent>
          ) : entry.status === "running" ? (
            "This subagent has not committed any activity yet."
          ) : (
            "This subagent committed no model or tool activity."
          )}
        </div>
      ) : (
        <ConversationTimeline
          key={entry.childId}
          active={active}
          items={items}
          selectedId={selectedId}
          actionsDisabled
          onSelect={onSelect}
          onAction={noopAction}
          hasOlder={hasOlder}
          olderLoading={olderLoading}
          olderError={olderError}
          onLoadOlder={onLoadOlder}
        />
      )}
    </div>
  );
}

function noopAction(): void {
  // A child scope has no card actions: approvals and retries belong to the
  // parent Agent, which owns the frozen attempt policy.
}
